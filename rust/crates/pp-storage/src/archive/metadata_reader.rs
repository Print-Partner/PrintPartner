use std::{
    cell::Cell,
    io::{self, Read},
};

use tar::{EntryType, Header};

const BLOCK_BYTES: usize = 512;
const MAX_EXTENSION_BYTES: u64 = 64 * 1024;

#[derive(Debug, PartialEq, Eq)]
enum Frame {
    Header(usize),
    Payload(u64),
    Member,
}

fn padded_size(size: u64) -> io::Result<u64> {
    size.checked_next_multiple_of(BLOCK_BYTES as u64)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Backup entry size overflow"))
}

fn after_header(kind: EntryType, size: impl FnOnce() -> io::Result<u64>) -> io::Result<Frame> {
    match kind {
        EntryType::GNULongName
        | EntryType::GNULongLink
        | EntryType::XHeader
        | EntryType::XGlobalHeader => {
            let size = size()?;
            if size > MAX_EXTENSION_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "Backup tar extension {kind:?} exceeds {MAX_EXTENSION_BYTES} byte limit"
                    ),
                ));
            }
            Ok(Frame::Payload(padded_size(size)?))
        }
        // The backup entry guard already refuses sparse members. Refuse the
        // header here, before tar can read their extension blocks.
        EntryType::GNUSparse => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Unsupported backup entry type: GNU sparse",
        )),
        _ => Ok(Frame::Member),
    }
}

/// Checks each header before returning it to tar. Tar consumes GNU long names
/// and local PAX metadata inside its iterator, before yielding the file.
/// For an ordinary member the caller supplies tar's effective size, which
/// can include a PAX size override. This keeps header checks aligned with
/// the same stream and entry boundaries tar uses for extraction.
pub(super) struct MetadataReader<'a, R> {
    inner: R,
    member_size: &'a Cell<Option<u64>>,
    frame: Frame,
    header: [u8; BLOCK_BYTES],
}

impl<'a, R> MetadataReader<'a, R> {
    pub(super) fn new(inner: R, member_size: &'a Cell<Option<u64>>) -> Self {
        Self {
            inner,
            member_size,
            frame: Frame::Header(0),
            header: [0; BLOCK_BYTES],
        }
    }

    pub(super) fn into_inner(self) -> R {
        self.inner
    }
}

impl<R: Read> Read for MetadataReader<'_, R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        loop {
            match self.frame {
                Frame::Header(used) => {
                    let end = used + output.len().min(BLOCK_BYTES - used);
                    let read = self.inner.read(&mut self.header[used..end])?;
                    let filled = used + read;
                    self.frame = if filled == BLOCK_BYTES {
                        let header = Header::from_byte_slice(&self.header);
                        self.member_size.set(None);
                        after_header(header.entry_type(), || header.entry_size())?
                    } else {
                        Frame::Header(filled)
                    };
                    output[..read].copy_from_slice(&self.header[used..filled]);
                    return Ok(read);
                }
                Frame::Member => {
                    let size = self.member_size.take().ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Backup tar member size unavailable",
                        )
                    })?;
                    self.frame = Frame::Payload(padded_size(size)?);
                }
                Frame::Payload(0) => self.frame = Frame::Header(0),
                Frame::Payload(remaining) => {
                    let len = output.len().min(remaining.try_into().unwrap_or(usize::MAX));
                    let read = self.inner.read(&mut output[..len])?;
                    self.frame = Frame::Payload(remaining - read as u64);
                    return Ok(read);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_payloads_are_capped_before_admission() {
        for kind in [
            EntryType::GNULongName,
            EntryType::GNULongLink,
            EntryType::XHeader,
            EntryType::XGlobalHeader,
        ] {
            assert_eq!(after_header(kind, || Ok(0)).unwrap(), Frame::Payload(0));
            assert_eq!(
                after_header(kind, || Ok(MAX_EXTENSION_BYTES - 1)).unwrap(),
                Frame::Payload(MAX_EXTENSION_BYTES)
            );
            assert_eq!(
                after_header(kind, || Ok(MAX_EXTENSION_BYTES)).unwrap(),
                Frame::Payload(MAX_EXTENSION_BYTES)
            );
            for size in [MAX_EXTENSION_BYTES + 1, u64::MAX] {
                let error = after_header(kind, || Ok(size)).unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::InvalidData);
                assert!(error.to_string().contains("byte limit"), "{error}");
            }
        }
    }

    #[test]
    fn regular_members_use_the_size_resolved_by_tar() {
        assert_eq!(
            after_header(EntryType::Regular, || panic!(
                "use the effective member size"
            ))
            .unwrap(),
            Frame::Member
        );
        assert_eq!(padded_size(MAX_EXTENSION_BYTES + 1).unwrap(), 65_536 + 512);
    }

    #[test]
    fn sparse_headers_are_refused_before_parsing_extensions() {
        let error =
            after_header(EntryType::GNUSparse, || panic!("refuse the member type")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("GNU sparse"), "{error}");
    }
}
