//! `GET /api/v2/plans/{id}/exports/stl-pack` (GRE-1525): a stored-entry ZIP
//! with one STL per Required unit, built from the accepted Plan on request.
//! Every distinct STL is opened and checked against its accepted SHA-256
//! before the first byte; then the archive streams from those open files on
//! a blocking task. Nothing is written to disk and no job runs.

use super::{App, ExportFailure, accepted_snapshot};
use axum::{
    body::{Body, Bytes},
    extract::{Path, Query, Request, State},
    http::{StatusCode, header},
    response::Response,
};
use http_body::Frame;
use pp_services::exports::pack::{self, Entry, LIMITS, PackPart, PackUnit, Scope, ZipStream};
use pp_storage::read_model::{Artifact, Part, ReadClient, Snapshot};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    collections::HashMap,
    fmt,
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    pin::Pin,
    task::{Context, Poll},
};
use tokio::sync::mpsc;

#[derive(Deserialize)]
pub(super) struct PackQuery {
    scope: Option<String>,
}

/// A Source file the pack cannot use, as the 409 problem lists it.
#[derive(Debug, Serialize)]
pub(super) struct UnavailableFile {
    relative_path: String,
    source: String,
    reason: &'static str,
}

pub(super) async fn stl_pack(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(query): Query<PackQuery>,
    request: Request,
) -> Response {
    let scope = match query.scope.as_deref() {
        None | Some("all") => Scope::All,
        Some("remaining") => Scope::Remaining,
        Some(_) => return ExportFailure::InvalidScope.response(),
    };
    let snapshot = match accepted_snapshot(&app, &id, request).await {
        Ok(snapshot) => snapshot,
        Err(failure) => return failure.response(),
    };
    let client = app.client.clone();
    let prepared = app
        .gate
        .blocking(move |_, _| prepare(&client, &snapshot, scope))
        .await;
    match prepared {
        Ok(Ok(prepared)) => stream(prepared),
        Ok(Err(failure)) => failure.response(),
        Err(_) => ExportFailure::Interrupted.response(),
    }
}

/// A verified Source file, open and ready to copy into the archive.
struct Source {
    file: File,
    length: u64,
    expected_sha256: String,
    source: String,
    relative_path: String,
}

struct Prepared {
    filename: String,
    entries: Vec<Entry>,
    /// The Source of each Part `entries` index.
    part_sources: Vec<usize>,
    sources: Vec<Source>,
}

/// Opens and verifies each distinct STL the scope needs, and lays out the
/// entries. Any file that fails refuses the whole pack.
fn prepare(
    client: &ReadClient,
    snapshot: &Snapshot,
    scope: Scope,
) -> Result<Prepared, ExportFailure> {
    let in_scope = |part: &&Part| {
        part.included
            && part
                .units
                .iter()
                .any(|u| scope == Scope::All || !u.completed)
    };
    let parts: Vec<&Part> = snapshot.parts.iter().filter(in_scope).collect();
    if parts.is_empty() {
        return Err(ExportFailure::NothingToPack);
    }
    let mut opened: HashMap<&Artifact, Option<usize>> = HashMap::new();
    let (mut sources, mut unavailable) = (Vec::new(), Vec::new());
    for part in &parts {
        if opened.contains_key(&part.artifact) {
            continue;
        }
        let index = match (client.open_artifact(&part.artifact), &part.artifact) {
            (
                Ok((file, length)),
                Artifact::Tracked {
                    expected_sha256, ..
                },
            ) => {
                sources.push(Source {
                    file,
                    length,
                    expected_sha256: expected_sha256.clone(),
                    source: part.source_layer.clone(),
                    relative_path: part.relative_path.clone(),
                });
                Some(sources.len() - 1)
            }
            (result, _) => {
                let reason = result.err().map_or("unavailable", |failure| failure.code());
                unavailable.push(UnavailableFile {
                    relative_path: part.relative_path.clone(),
                    source: part.source_layer.clone(),
                    reason,
                });
                None
            }
        };
        opened.insert(&part.artifact, index);
    }
    if !unavailable.is_empty() {
        return Err(ExportFailure::SourceFilesUnavailable(unavailable));
    }
    let part_sources: Vec<usize> = parts
        .iter()
        .map(|part| opened[&part.artifact].unwrap_or_default())
        .collect();
    let pack_parts: Vec<PackPart> = parts
        .iter()
        .zip(&part_sources)
        .map(|(part, &source)| PackPart {
            role: part.effective_role.clone(),
            relative_path: part.relative_path.clone(),
            filename: part.filename.clone(),
            size: sources[source].length,
            units: part
                .units
                .iter()
                .map(|unit| PackUnit {
                    number: u32::try_from(unit.unit_index + 1).unwrap_or(u32::MAX),
                    printed: unit.completed,
                })
                .collect(),
        })
        .collect();
    let entries = pack::layout(&pack_parts, scope, LIMITS).map_err(ExportFailure::Pack)?;
    Ok(Prepared {
        filename: pack::filename(&snapshot.profile.name, snapshot.plan_version),
        entries,
        part_sources,
        sources,
    })
}

/// Answers 200 at once and writes the archive from a blocking task. The body
/// is chunked: no Content-Length.
fn stream(prepared: Prepared) -> Response {
    let (sender, receiver) = mpsc::channel(4);
    let filename = prepared.filename.clone();
    tokio::task::spawn_blocking(move || write_pack(prepared, sender));
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/zip")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(Body::new(ChannelBody(receiver)))
        .unwrap_or_else(|_| ExportFailure::Interrupted.response())
}

fn write_pack(mut prepared: Prepared, sender: mpsc::Sender<io::Result<Bytes>>) {
    let mut zip = ZipStream::new(ChannelWriter::new(sender.clone()));
    let mut written = Ok(());
    for entry in &prepared.entries {
        let source = &mut prepared.sources[prepared.part_sources[entry.part]];
        written = source
            .file
            .seek(SeekFrom::Start(0))
            .and_then(|_| zip.entry(&entry.name, &mut Verifying::new(source)));
        if written.is_err() {
            break;
        }
    }
    let error = match written.and_then(|()| zip.finish()) {
        Ok(_) => return,
        Err(error) => error,
    };
    if let Some(changed) = error
        .get_ref()
        .and_then(|e| e.downcast_ref::<SourceChanged>())
    {
        log::warn!(target: "pp_api::exports", "{changed}");
    }
    // Ending the body with an error stops the transfer: the client keeps a
    // broken archive, never one with wrong bytes. A closed channel means the
    // client left, and there is no one to tell.
    let _ = sender.blocking_send(Err(error));
}

/// A Source file whose bytes no longer hash to the accepted SHA-256 while
/// the pack streams it.
#[derive(Debug)]
pub(super) struct SourceChanged {
    source: String,
    relative_path: String,
}

impl fmt::Display for SourceChanged {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "STL pack stopped: Source file {} in {} changed during the download",
            self.relative_path, self.source
        )
    }
}

impl std::error::Error for SourceChanged {}

/// Reads exactly a Source's verified length, hashing as it goes, and fails
/// at the end unless the bytes still match the accepted SHA-256.
struct Verifying<'a> {
    source: &'a mut Source,
    left: u64,
    hash: Sha256,
}

impl<'a> Verifying<'a> {
    fn new(source: &'a mut Source) -> Self {
        let left = source.length;
        Self {
            source,
            left,
            hash: Sha256::new(),
        }
    }

    fn changed(&self) -> io::Error {
        io::Error::other(SourceChanged {
            source: self.source.source.clone(),
            relative_path: self.source.relative_path.clone(),
        })
    }
}

impl Read for Verifying<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.left == 0 {
            let hash = std::mem::take(&mut self.hash);
            return if hex::encode(hash.finalize()) == self.source.expected_sha256 {
                Ok(0)
            } else {
                Err(self.changed())
            };
        }
        let want = buffer
            .len()
            .min(usize::try_from(self.left).unwrap_or(usize::MAX));
        let read = self.source.file.read(&mut buffer[..want])?;
        if read == 0 {
            // Shorter than when it was checked.
            return Err(self.changed());
        }
        self.hash.update(&buffer[..read]);
        self.left -= read as u64;
        Ok(read)
    }
}

/// Collects archive bytes into 64 KiB chunks for the response body. A send
/// fails when the client has gone, which stops the writer.
struct ChannelWriter {
    sender: mpsc::Sender<io::Result<Bytes>>,
    buffer: Vec<u8>,
}

const CHUNK: usize = 64 * 1024;

impl ChannelWriter {
    fn new(sender: mpsc::Sender<io::Result<Bytes>>) -> Self {
        Self {
            sender,
            buffer: Vec::with_capacity(CHUNK),
        }
    }

    fn send(&mut self) -> io::Result<()> {
        let chunk = Bytes::from(std::mem::replace(
            &mut self.buffer,
            Vec::with_capacity(CHUNK),
        ));
        self.sender
            .blocking_send(Ok(chunk))
            .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))
    }
}

impl Write for ChannelWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let room = CHUNK - self.buffer.len();
        let taken = bytes.len().min(room);
        self.buffer.extend_from_slice(&bytes[..taken]);
        if self.buffer.len() == CHUNK {
            self.send()?;
        }
        Ok(taken)
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.buffer.is_empty() {
            Ok(())
        } else {
            self.send()
        }
    }
}

/// The response body: the chunks the writer sends, ending when it drops its
/// sender or sends an error.
struct ChannelBody(mpsc::Receiver<io::Result<Bytes>>);

impl http_body::Body for ChannelBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        self.0
            .poll_recv(cx)
            .map(|chunk| chunk.map(|chunk| chunk.map(Frame::data)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-entry pack over a file holding `text`, checked against `accepted`.
    fn prepared(text: &[u8], accepted: &[u8]) -> Prepared {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(text).unwrap();
        Prepared {
            filename: "kit-v1-stl.zip".into(),
            entries: vec![Entry {
                name: "primary/_root/arm_01.stl".into(),
                part: 0,
            }],
            part_sources: vec![0],
            sources: vec![Source {
                file,
                length: text.len() as u64,
                expected_sha256: hex::encode(Sha256::digest(accepted)),
                source: "base:Kit".into(),
                relative_path: "arm.stl".into(),
            }],
        }
    }

    /// Everything the body would carry, and the error that ended it.
    fn written(prepared: Prepared) -> (Vec<u8>, Option<io::Error>) {
        let (sender, mut receiver) = mpsc::channel(64);
        write_pack(prepared, sender);
        let mut body = Vec::new();
        while let Ok(chunk) = receiver.try_recv() {
            match chunk {
                Ok(bytes) => body.extend_from_slice(&bytes),
                Err(error) => return (body, Some(error)),
            }
        }
        (body, None)
    }

    #[test]
    fn an_unchanged_file_completes_the_archive() {
        let (body, error) = written(prepared(b"solid arm\n", b"solid arm\n"));
        assert!(error.is_none(), "{error:?}");
        let mut zip = zip::ZipArchive::new(io::Cursor::new(body)).unwrap();
        let mut entry = zip.by_index(0).unwrap();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"solid arm\n");
    }

    #[test]
    fn a_file_changed_after_its_check_stops_the_transfer() {
        // Same length as when it was checked, different bytes.
        let (body, error) = written(prepared(b"solid bad\n", b"solid arm\n"));
        let error = error.expect("the body ends with an error");
        let changed = error
            .get_ref()
            .and_then(|e| e.downcast_ref::<SourceChanged>());
        let changed = changed.expect("a typed SourceChanged");
        assert_eq!(
            (changed.source.as_str(), changed.relative_path.as_str()),
            ("base:Kit", "arm.stl")
        );
        assert!(
            zip::ZipArchive::new(io::Cursor::new(body)).is_err(),
            "a broken archive"
        );
    }

    #[test]
    fn a_shortened_file_stops_the_transfer() {
        let short = prepared(b"solid arm\n", b"solid arm\n");
        short.sources[0].file.set_len(4).unwrap();
        let (_, error) = written(short);
        let error = error.expect("the body ends with an error");
        assert!(error.get_ref().is_some_and(|e| e.is::<SourceChanged>()));
    }
}
