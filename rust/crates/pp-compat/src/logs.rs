use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::redact;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as platform;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as platform;

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FailureReason {
    SpawnFailed,
    ReadinessTimeout,
    ChildExit,
    HealthLost,
    CleanupFailed,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub(crate) enum Termination {
    ExitCode(i32),
    Signal(i32),
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub(crate) enum Event {
    #[serde(rename = "compat_starting")]
    Starting,
    #[serde(rename = "compat_ready")]
    Ready,
    #[serde(rename = "compat_reaped")]
    Reaped,
    #[serde(rename = "compat_crash_guard")]
    CrashGuard,
    #[serde(rename = "compat_start_failed")]
    StartFailed,
    #[serde(rename = "compat_output")]
    Output,
    #[serde(rename = "compat_failure")]
    Failure {
        reason: FailureReason,
        termination: Option<Termination>,
    },
}

#[derive(Serialize, Deserialize)]
struct Record {
    time: u64,
    level: u64,
    #[serde(flatten)]
    event: Event,
}

pub fn recent_diagnostics(data: &Path) -> std::io::Result<String> {
    const ROW_BYTES: usize = 2048;
    const RECORDS: usize = 100;
    let directory = view_directory(data)?;
    let (records, incomplete) = tail_rows(&directory, DESKTOP, RECORDS, ROW_BYTES, |row| {
        serde_json::from_slice::<Record>(row)
            .ok()
            .filter(|record| [10, 20, 30, 40, 50, 60].contains(&record.level))
    })?;
    let mut text = String::new();
    if incomplete {
        text.push_str(OMITTED);
    }
    if records.is_empty() {
        text.push_str("No diagnostic events recorded.\n");
    }
    for record in records {
        text.push_str(&serde_json::to_string(&record)?);
        text.push('\n');
    }
    Ok(text)
}

/// The Logs view: supervisor events, then the newest compat.log and core.log
/// rows. Rows are scrubbed again here, so rows written before a scrub rule
/// existed do not reach the UI either.
pub fn recent_logs(data: &Path) -> std::io::Result<String> {
    const RECORDS: usize = 40;
    let mut text = String::from("Service events (desktop.jsonl)\n\n");
    text.push_str(&recent_diagnostics(data)?);
    let directory = view_directory(data)?;
    let scrubber = redact::Scrubber::default();
    for (title, stream) in [
        ("Compat backend (compat.log)", COMPAT),
        ("Core (core.log)", CORE),
    ] {
        let (rows, incomplete) = tail_rows(&directory, stream, RECORDS, LINE_BYTES, |row| {
            let mut value = serde_json::from_slice::<serde_json::Value>(row).ok()?;
            let level = value.as_object()?.get("level")?.as_u64()?;
            if ![10, 20, 30, 40, 50, 60].contains(&level) {
                return None;
            }
            scrubber.json(&mut value);
            Some(value.to_string())
        })?;
        text.push_str(&format!("\n{title}\n\n"));
        if incomplete {
            text.push_str(OMITTED);
        }
        if rows.is_empty() {
            text.push_str("No records yet.\n");
        }
        for row in rows {
            text.push_str(&row);
            text.push('\n');
        }
    }
    Ok(text)
}

/// The admitted `<data>/logs` directory, for the Reveal log folder command.
/// Refuses a relative path, which an opener could read as an option, a
/// symlink, anything other than a directory owned by this user, and access
/// for any other account: group or world mode bits on Unix, an ACE for another
/// account on Windows.
pub fn log_folder(data: &Path) -> std::io::Result<PathBuf> {
    let directory = data.join("logs");
    if !directory.is_absolute() {
        return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
    }
    let metadata = std::fs::symlink_metadata(&directory)?;
    if !metadata.is_dir() || !platform::private_folder(&directory, &metadata)? {
        return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
    }
    Ok(directory)
}

const OMITTED: &str = "Showing recent safe records. Older or invalid records were omitted.\n\n";

fn view_directory(data: &Path) -> std::io::Result<PathBuf> {
    let directory = data.join("logs");
    if std::fs::symlink_metadata(&directory)?
        .file_type()
        .is_symlink()
    {
        return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
    }
    Ok(directory)
}

/// The newest complete rows of `stream` that `admit` accepts, oldest archive
/// first, reading at most 64 KiB from the end of each file. The flag is set
/// when a file, row or record was skipped.
fn tail_rows<T>(
    directory: &Path,
    stream: Stream,
    records: usize,
    row_bytes: usize,
    mut admit: impl FnMut(&[u8]) -> Option<T>,
) -> std::io::Result<(std::collections::VecDeque<T>, bool)> {
    const TAIL_BYTES: u64 = 64 * 1024;
    let mut rows = std::collections::VecDeque::new();
    let mut incomplete = false;
    for index in (0..=ARCHIVES).rev() {
        let name = if index == 0 {
            format!("{}.{}", stream.stem, stream.ext)
        } else {
            format!("{}.{index}.{}", stream.stem, stream.ext)
        };
        let mut file = match platform::open_tail(&directory.join(name)) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                incomplete = true;
                continue;
            }
        };
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            incomplete = true;
            continue;
        }
        let offset = metadata.len().saturating_sub(TAIL_BYTES);
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = Vec::new();
        file.take(TAIL_BYTES).read_to_end(&mut bytes)?;
        incomplete |= offset != 0;
        let mut start = offset == 0;
        for row in bytes.split_inclusive(|byte| *byte == b'\n') {
            if !start {
                start = true;
                continue;
            }
            if row.len() > row_bytes || row.last() != Some(&b'\n') {
                incomplete = true;
                continue;
            }
            let Some(row) = admit(row) else {
                incomplete = true;
                continue;
            };
            if rows.len() == records {
                rows.pop_front();
                incomplete = true;
            }
            rows.push_back(row);
        }
    }
    Ok((rows, incomplete))
}

/// One rotated log: `<stem>.<ext>` plus archives `<stem>.1.<ext>` through
/// `<stem>.6.<ext>`. Each file rotates daily or before it would pass 10 MiB.
#[derive(Clone, Copy)]
pub(crate) struct Stream {
    stem: &'static str,
    ext: &'static str,
}

/// Supervisor lifecycle events read back by the Logs view.
pub(crate) const DESKTOP: Stream = Stream {
    stem: "desktop",
    ext: "jsonl",
};
/// Scrubbed pino JSON and stdout/stderr text from the compat backend.
pub(crate) const COMPAT: Stream = Stream {
    stem: "compat",
    ext: "log",
};
/// Scrubbed Rust tracing events and panics from the core process.
pub(crate) const CORE: Stream = Stream {
    stem: "core",
    ext: "log",
};

const ARCHIVES: usize = 6;
const FILE_BYTES: u64 = 10 * 1024 * 1024;
/// Longest captured line; longer output is cut so one child write cannot
/// grow memory or a log row without bound.
const LINE_BYTES: usize = 8192;

impl Stream {
    fn active(self) -> String {
        format!("{}.{}", self.stem, self.ext)
    }
    fn archive(self, index: usize) -> String {
        format!("{}.{index}.{}", self.stem, self.ext)
    }
}

#[derive(Clone)]
pub(crate) struct Logs {
    events: Arc<Mutex<LogFile>>,
    compat: Arc<Mutex<LogFile>>,
    scrubber: redact::Scrubber,
}
struct LogFile {
    stream: Stream,
    directory: platform::Directory,
    /// `None` after an unlinked log was closed and its replacement failed.
    file: Option<File>,
    day: u64,
    bytes: u64,
}

fn secure_archives(directory: &platform::Directory, stream: Stream) -> std::io::Result<()> {
    for index in 1..=ARCHIVES {
        platform::secure_archive(directory, &stream.archive(index))?;
    }
    Ok(())
}

/// Create exclusively, then open an existing entry without creating it.
///
/// Another process can remove desktop.jsonl during this open, and the open
/// then fails closed instead of retrying. An unlink or rename between EEXIST
/// and the reopen returns ENOENT. An unlink after the open leaves no link
/// (on Windows, a pending delete), which `secure_log` refuses with
/// InvalidInput. A rename after
/// the open keeps `nlink == 1`, so the open succeeds on the renamed file.
/// After either failure, startup fails and `Logs::event` drops its event. That
/// is intended: the storage lease admits one owner per data directory and
/// rotation holds the log mutex through its reopen, so only a writer outside
/// that owner can reach either window.
fn open_log(directory: &platform::Directory, stream: Stream) -> std::io::Result<File> {
    open_log_observed(directory, stream, |_| {})
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum OpenWindow {
    /// The exclusive create found an entry and the reopen without create is next.
    Reopen,
    /// The entry is open and `secure_log` is next.
    Secure,
}

fn open_log_observed(
    directory: &platform::Directory,
    stream: Stream,
    mut window: impl FnMut(OpenWindow),
) -> std::io::Result<File> {
    let name = stream.active();
    let file = match platform::open_append(directory, &name, true) {
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            window(OpenWindow::Reopen);
            platform::open_append(directory, &name, false)?
        }
        result => result?,
    };
    window(OpenWindow::Secure);
    platform::secure_log(&file)?;
    Ok(file)
}

impl LogFile {
    fn open(directory: &Path, stream: Stream) -> std::io::Result<Self> {
        let directory = platform::open_directory(directory)?;
        platform::secure_directory(&directory)?;
        secure_archives(&directory, stream)?;
        let file = open_log(&directory, stream)?;
        let bytes = file.metadata()?.len();
        Ok(Self {
            stream,
            directory,
            file: Some(file),
            day: now_secs() / 86400,
            bytes,
        })
    }

    /// Append one complete row, rotating first when the day changed or the
    /// row would pass the size cap. Failures drop the row; logging never
    /// stops the caller.
    fn append(&mut self, row: &[u8], now: u64) {
        let stream = self.stream;
        if platform::secure_directory(&self.directory).is_err() {
            return;
        }
        if self.file.as_ref().is_none_or(platform::unlinked) {
            // Close first: a classic Windows delete keeps the name taken while
            // a handle is open, so the replacement could not be created.
            self.file = None;
            let Ok(file) = open_log(&self.directory, stream) else {
                return;
            };
            let Ok(metadata) = file.metadata() else {
                return;
            };
            self.bytes = metadata.len();
            self.day = now / 86400;
            self.file = Some(file);
        }
        if self
            .file
            .as_ref()
            .is_none_or(|file| platform::secure_log(file).is_err())
        {
            return;
        }
        if self.day != now / 86400 || self.bytes + row.len() as u64 > FILE_BYTES {
            let rotate = (|| -> std::io::Result<()> {
                open_log(&self.directory, stream)?;
                secure_archives(&self.directory, stream)?;
                for index in (1..=ARCHIVES).rev() {
                    let source = if index == 1 {
                        stream.active()
                    } else {
                        stream.archive(index - 1)
                    };
                    match platform::rename(&self.directory, &source, &stream.archive(index)) {
                        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                            return Err(error);
                        }
                        _ => {}
                    }
                }
                self.file = Some(open_log(&self.directory, stream)?);
                self.bytes = 0;
                self.day = now / 86400;
                Ok(())
            })();
            if rotate.is_err() {
                return;
            }
        }
        if let Some(file) = self.file.as_mut()
            && file.write_all(row).is_ok()
        {
            self.bytes += row.len() as u64;
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl Logs {
    pub(crate) fn open(directory: PathBuf) -> std::io::Result<Self> {
        Ok(Self {
            events: Arc::new(Mutex::new(LogFile::open(&directory, DESKTOP)?)),
            compat: Arc::new(Mutex::new(LogFile::open(&directory, COMPAT)?)),
            scrubber: redact::Scrubber::default(),
        })
    }

    /// A capture handle that also scrubs a secret handed to this child.
    pub(crate) fn with_secret(&self, secret: impl Into<String>) -> Self {
        Self {
            scrubber: self.scrubber.clone().with_secret(secret),
            ..self.clone()
        }
    }

    pub(crate) fn event(&self, event: Event, level: u64) {
        let now = now_secs();
        let Ok(mut row) = serde_json::to_string(&Record {
            time: now,
            event,
            level,
        }) else {
            return;
        };
        row.push('\n');
        let Ok(mut log) = self.events.lock() else {
            return;
        };
        log.append(row.as_bytes(), now);
    }

    pub(crate) async fn capture(&self, reader: impl tokio::io::AsyncRead + Unpin) {
        let mut reader = BufReader::with_capacity(4096, reader);
        let mut line = Vec::new();
        loop {
            let Ok(bytes) = reader.fill_buf().await else {
                return;
            };
            if bytes.is_empty() {
                if !line.is_empty() {
                    self.capture_line(&line);
                }
                return;
            }
            for byte in bytes {
                if *byte == b'\n' {
                    self.capture_line(&line);
                    line.clear();
                } else if line.len() < LINE_BYTES {
                    line.push(*byte);
                }
            }
            let consumed = bytes.len();
            reader.consume(consumed);
        }
    }

    fn capture_line(&self, line: &[u8]) {
        let now = now_secs();
        let mut value = match serde_json::from_slice(line) {
            Ok(serde_json::Value::Object(record)) => serde_json::Value::Object(record),
            _ => serde_json::json!({
                "time": now * 1000,
                "level": 30,
                "msg": String::from_utf8_lossy(line),
            }),
        };
        let level = value["level"]
            .as_u64()
            .filter(|n| [10, 20, 30, 40, 50, 60].contains(n))
            .unwrap_or(30);
        let event = match value["event"].as_str() {
            Some("desktop_start_failed") => Event::StartFailed,
            _ => Event::Output,
        };
        self.event(event, level);
        self.scrubber.json(&mut value);
        let mut row = value.to_string();
        row.push('\n');
        if let Ok(mut log) = self.compat.lock() {
            log.append(row.as_bytes(), now);
        }
    }
}

/// Rust `tracing` subscriber that writes scrubbed JSON rows to `core.log`
/// beside the compat logs, with the same rotation. Levels use pino numbers so
/// both sources read alike. Spans are accepted and not recorded.
/// `time` is epoch seconds here, while compat.log keeps pino's epoch
/// milliseconds; a combined view must normalize.
#[derive(Clone)]
pub struct CoreLog {
    sink: Arc<Mutex<Sink>>,
    scrubber: redact::Scrubber,
}

/// Rows emitted before the data directory is admitted wait in memory, at
/// most [`PENDING_ROWS`] of them, and reach `core.log` on [`CoreLog::attach`].
enum Sink {
    Pending {
        rows: std::collections::VecDeque<String>,
        dropped: u64,
    },
    File(LogFile),
}

const PENDING_ROWS: usize = 256;

impl CoreLog {
    /// A subscriber that holds rows until [`CoreLog::attach`]. Install it
    /// first thing so startup events are not lost.
    pub fn buffered() -> Self {
        Self {
            sink: Arc::new(Mutex::new(Sink::Pending {
                rows: std::collections::VecDeque::new(),
                dropped: 0,
            })),
            scrubber: redact::Scrubber::default(),
        }
    }

    /// Open `<data>/logs/core.log` with the log directory checks used for
    /// every other stream.
    pub fn open(data: &Path) -> std::io::Result<Self> {
        let log = Self::buffered();
        log.attach(data)?;
        Ok(log)
    }

    /// Start writing to `<data>/logs/core.log`, first flushing the rows held
    /// since [`CoreLog::buffered`]. Call once the data directory is admitted.
    pub fn attach(&self, data: &Path) -> std::io::Result<()> {
        let mut file = LogFile::open(&data.join("logs"), CORE)?;
        let Ok(mut sink) = self.sink.lock() else {
            return Err(std::io::Error::other("core log poisoned"));
        };
        if let Sink::Pending { rows, dropped } = &mut *sink {
            let now = now_secs();
            if *dropped > 0 {
                let notice = serde_json::json!({
                    "time": now, "level": 40, "target": module_path!(),
                    "message": format!("core log dropped {dropped} early rows"),
                });
                file.append(format!("{notice}\n").as_bytes(), now);
            }
            for row in rows.drain(..) {
                file.append(row.as_bytes(), now);
            }
        }
        *sink = Sink::File(file);
        Ok(())
    }

    /// Record panics before the previous hook runs, then make this the
    /// process-wide subscriber. Local crash records do not depend on
    /// telemetry. The panic hook is armed even when another subscriber was
    /// already installed; that case returns false.
    pub fn install(&self) -> bool {
        let log = self.clone();
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            log.panic(info);
            previous(info);
        }));
        tracing::subscriber::set_global_default(self.clone()).is_ok()
    }

    fn panic(&self, info: &std::panic::PanicHookInfo<'_>) {
        let payload = info.payload();
        let message = payload
            .downcast_ref::<&str>()
            .map(|text| (*text).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic".to_owned());
        let mut fields = serde_json::Map::new();
        fields.insert("message".into(), message.into());
        if let Some(location) = info.location() {
            fields.insert(
                "location".into(),
                format!("{}:{}", location.file(), location.line()).into(),
            );
        }
        if let Some(thread) = std::thread::current().name() {
            fields.insert("thread".into(), thread.into());
        }
        self.write(60, "panic", fields);
    }

    fn write(&self, level: u64, target: &str, fields: serde_json::Map<String, serde_json::Value>) {
        let now = now_secs();
        let mut fields = serde_json::Value::Object(fields);
        self.scrubber.json(&mut fields);
        let mut record = match fields {
            serde_json::Value::Object(fields) => fields,
            _ => serde_json::Map::new(),
        };
        record.insert("time".into(), now.into());
        record.insert("level".into(), level.into());
        record.insert("target".into(), target.into());
        let record = serde_json::Value::Object(record);
        let mut row = record.to_string();
        if row.len() > LINE_BYTES {
            let mut message = record["message"].as_str().unwrap_or_default().to_owned();
            let mut end = LINE_BYTES / 2;
            while !message.is_char_boundary(end.min(message.len())) {
                end -= 1;
            }
            message.truncate(end);
            row = serde_json::json!({
                "time": now, "level": level, "target": target,
                "message": message, "truncated": true,
            })
            .to_string();
        }
        row.push('\n');
        let Ok(mut sink) = self.sink.lock() else {
            return;
        };
        match &mut *sink {
            Sink::File(log) => log.append(row.as_bytes(), now),
            Sink::Pending { rows, dropped } => {
                if rows.len() == PENDING_ROWS {
                    rows.pop_front();
                    *dropped += 1;
                }
                rows.push_back(row);
            }
        }
    }
}

struct Fields<'a> {
    map: &'a mut serde_json::Map<String, serde_json::Value>,
    scrubber: &'a redact::Scrubber,
}

impl tracing::field::Visit for Fields<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.map
            .insert(field.name().into(), format!("{value:?}").into());
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.map.insert(field.name().into(), value.into());
    }
    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.map.insert(field.name().into(), value.into());
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.map.insert(field.name().into(), value.into());
    }
    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.map.insert(field.name().into(), value.into());
    }
    fn record_error(
        &mut self,
        field: &tracing::field::Field,
        value: &(dyn std::error::Error + 'static),
    ) {
        self.map
            .insert(field.name().into(), self.scrubber.error(value).into());
    }
}

impl tracing::Subscriber for CoreLog {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        *metadata.level() <= tracing::Level::INFO
    }
    fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
        Some(tracing::level_filters::LevelFilter::INFO)
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let metadata = event.metadata();
        let level = match *metadata.level() {
            tracing::Level::ERROR => 50,
            tracing::Level::WARN => 40,
            tracing::Level::INFO => 30,
            tracing::Level::DEBUG => 20,
            tracing::Level::TRACE => 10,
        };
        let mut fields = serde_json::Map::new();
        event.record(&mut Fields {
            map: &mut fields,
            scrubber: &self.scrubber,
        });
        self.write(level, metadata.target(), fields);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform::test_support::{
        access, identity, path_identity, private, restrict, symlink_dir, symlink_file, widen,
        widened,
    };

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "pp-diagnostic-{}-{}",
                std::process::id(),
                rand::random::<u64>()
            ));
            std::fs::create_dir_all(path.join("logs")).unwrap();
            Self(path)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    #[cfg(unix)]
    fn chmod(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    fn force_rotation(logs: &Logs, size: bool) {
        let mut log = logs.events.lock().unwrap();
        if size {
            log.bytes = 10 * 1024 * 1024;
        } else {
            log.day = 0;
        }
    }

    #[test]
    fn ticket_t_2_unlinked_active_log_recovers_without_restart() {
        for rotation in [None, Some(false), Some(true)] {
            let scratch = Scratch::new();
            let directory = scratch.0.join("logs");
            let path = directory.join("desktop.jsonl");
            let logs = Logs::open(directory).unwrap();
            logs.event(Event::Starting, 30);
            std::fs::remove_file(&path).unwrap();
            if let Some(size) = rotation {
                force_rotation(&logs, size);
            }
            logs.event(Event::Ready, 30);
            let row: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(row["event"], "compat_ready");
            assert!(private(&path));
        }
    }

    /// The classic delete leaves the open log pending deletion under its name,
    /// so recovery must close it before creating the replacement.
    #[cfg(windows)]
    #[test]
    fn ticket_t_2_delete_pending_active_log_recovers_without_restart() {
        for rotation in [None, Some(false), Some(true)] {
            let scratch = Scratch::new();
            let directory = scratch.0.join("logs");
            let path = directory.join("desktop.jsonl");
            let logs = Logs::open(directory).unwrap();
            logs.event(Event::Starting, 30);
            platform::test_support::delete_pending(&path);
            assert!(
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                    .is_err()
            );
            if let Some(size) = rotation {
                force_rotation(&logs, size);
            }
            logs.event(Event::Ready, 30);
            let row: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(row["event"], "compat_ready");
            assert!(private(&path));
        }
    }

    /// Securing the log directory never touches the access of an entry that
    /// is a hard link to a file elsewhere, and the entry is refused.
    #[test]
    fn ticket_t_2_hardlinked_logs_leave_outside_access_unchanged() {
        for name in ["desktop.jsonl", "desktop.1.jsonl"] {
            let scratch = Scratch::new();
            let target = scratch.0.join("outside");
            std::fs::write(&target, b"private history\n").unwrap();
            widen(&target);
            let before = access(&target);
            std::fs::hard_link(&target, scratch.0.join("logs").join(name)).unwrap();
            assert!(Logs::open(scratch.0.join("logs")).is_err());
            assert_eq!(access(&target), before);
            assert_eq!(std::fs::read(&target).unwrap(), b"private history\n");
        }
    }

    #[test]
    fn ticket_t_2_unlinked_active_log_refuses_redirected_recovery() {
        for hardlink in [false, true] {
            let scratch = Scratch::new();
            let directory = scratch.0.join("logs");
            let path = directory.join("desktop.jsonl");
            let logs = Logs::open(directory).unwrap();
            std::fs::remove_file(&path).unwrap();
            let target = scratch.0.join("outside");
            std::fs::write(&target, b"private history\n").unwrap();
            widen(&target);
            if hardlink {
                std::fs::hard_link(&target, &path).unwrap();
            } else {
                symlink_file(&target, &path);
            }
            logs.event(Event::Ready, 30);
            assert_eq!(std::fs::read(&target).unwrap(), b"private history\n");
            assert!(widened(&target));
            std::fs::remove_file(&path).unwrap();
            logs.event(Event::Ready, 30);
            let row: serde_json::Value =
                serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            assert_eq!(row["event"], "compat_ready");
        }
    }

    /// Unix admits an unreadable archive whose mode is within 0600 without
    /// opening it. Windows opens every archive through its owner's implicit
    /// `WRITE_DAC`; see the next test.
    #[cfg(unix)]
    #[test]
    fn ticket_t_2_restrictive_archives_allow_startup_and_rotation() {
        for permissions in [0o000, 0o200] {
            for size in [false, true] {
                let scratch = Scratch::new();
                let directory = scratch.0.join("logs");
                let path = directory.join("desktop.1.jsonl");
                std::fs::write(&path, b"history\n").unwrap();
                chmod(&path, permissions);
                let logs = Logs::open(directory.clone()).unwrap();
                logs.event(Event::Starting, 30);
                force_rotation(&logs, size);
                logs.event(Event::Ready, 30);
                let archived = directory.join("desktop.2.jsonl");
                assert_eq!(mode(&archived) & !0o600, 0);
                chmod(&archived, 0o600);
                assert_eq!(std::fs::read(&archived).unwrap(), b"history\n");
                let row: serde_json::Value = serde_json::from_slice(
                    &std::fs::read(directory.join("desktop.jsonl")).unwrap(),
                )
                .unwrap();
                assert_eq!(row["event"], "compat_ready");
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn ticket_t_2_archive_without_access_is_repaired_for_startup_and_rotation() {
        for size in [false, true] {
            let scratch = Scratch::new();
            let directory = scratch.0.join("logs");
            let path = directory.join("desktop.1.jsonl");
            std::fs::write(&path, b"history\n").unwrap();
            platform::test_support::deny_all(&path);
            assert!(std::fs::read(&path).is_err());
            let logs = Logs::open(directory.clone()).unwrap();
            assert!(private(&path));
            logs.event(Event::Starting, 30);
            force_rotation(&logs, size);
            logs.event(Event::Ready, 30);
            let archived = directory.join("desktop.2.jsonl");
            assert!(private(&archived));
            assert_eq!(std::fs::read(&archived).unwrap(), b"history\n");
            let row: serde_json::Value =
                serde_json::from_slice(&std::fs::read(directory.join("desktop.jsonl")).unwrap())
                    .unwrap();
            assert_eq!(row["event"], "compat_ready");
        }
    }

    /// Unix-only for the same reason as the restrictive-archive test above.
    #[cfg(unix)]
    #[test]
    fn ticket_t_2_restrictive_hardlinked_archive_is_still_refused() {
        let scratch = Scratch::new();
        let target = scratch.0.join("outside");
        std::fs::write(&target, b"history\n").unwrap();
        chmod(&target, 0o000);
        std::fs::hard_link(&target, scratch.0.join("logs/desktop.1.jsonl")).unwrap();
        assert!(Logs::open(scratch.0.join("logs")).is_err());
        assert_eq!(mode(&target), 0o000);
        chmod(&target, 0o600);
        assert_eq!(std::fs::read(target).unwrap(), b"history\n");
    }

    #[test]
    fn ticket_t_2_hardlink_active_startup_leaves_target_unchanged() {
        let scratch = Scratch::new();
        let target = scratch.0.join("outside");
        std::fs::write(&target, b"private history\n").unwrap();
        widen(&target);
        std::fs::hard_link(&target, scratch.0.join("logs/desktop.jsonl")).unwrap();
        let result = Logs::open(scratch.0.join("logs"));
        if let Ok(logs) = &result {
            logs.event(Event::Ready, 30);
        }
        assert_eq!(std::fs::read(&target).unwrap(), b"private history\n");
        assert!(widened(&target));
        assert!(result.is_err());
    }

    #[test]
    fn ticket_t_2_hardlink_active_after_startup_refuses_append_and_both_rotations() {
        for rotation in [None, Some(false), Some(true)] {
            let scratch = Scratch::new();
            let directory = scratch.0.join("logs");
            let path = directory.join("desktop.jsonl");
            let logs = Logs::open(directory.clone()).unwrap();
            logs.event(Event::Starting, 30);
            let before = std::fs::read(&path).unwrap();
            let target = scratch.0.join("outside");
            std::fs::hard_link(&path, &target).unwrap();
            widen(&target);
            if let Some(size) = rotation {
                force_rotation(&logs, size);
            }
            for _ in 0..2 {
                logs.event(Event::Ready, 30);
            }
            assert_eq!(std::fs::read(&target).unwrap(), before, "{rotation:?}");
            assert!(widened(&target));
            assert!(!directory.join("desktop.1.jsonl").exists());
        }
    }

    #[test]
    fn ticket_t_2_hardlink_active_replacement_preserves_history_on_rotation() {
        for size in [false, true] {
            let scratch = Scratch::new();
            let directory = scratch.0.join("logs");
            let path = directory.join("desktop.jsonl");
            let logs = Logs::open(directory.clone()).unwrap();
            logs.event(Event::Starting, 30);
            let original = scratch.0.join("original");
            std::fs::rename(&path, &original).unwrap();
            let before = std::fs::read(&original).unwrap();
            let target = scratch.0.join("outside");
            std::fs::write(&target, b"private history\n").unwrap();
            widen(&target);
            std::fs::hard_link(&target, &path).unwrap();
            std::fs::write(directory.join("desktop.6.jsonl"), b"oldest\n").unwrap();
            force_rotation(&logs, size);
            logs.event(Event::Ready, 30);
            assert_eq!(std::fs::read(&original).unwrap(), before);
            assert_eq!(std::fs::read(&target).unwrap(), b"private history\n");
            assert!(widened(&target));
            assert_eq!(
                std::fs::read(directory.join("desktop.6.jsonl")).unwrap(),
                b"oldest\n"
            );
            assert!(!directory.join("desktop.1.jsonl").exists());
        }
    }

    #[test]
    fn ticket_t_2_hardlink_archive_startup_rejects_every_slot_without_creating_active() {
        for slot in 1..=6 {
            let scratch = Scratch::new();
            let directory = scratch.0.join("logs");
            let target = scratch.0.join("outside");
            std::fs::write(&target, b"archive\n").unwrap();
            widen(&target);
            std::fs::hard_link(&target, directory.join(format!("desktop.{slot}.jsonl"))).unwrap();
            assert!(Logs::open(directory.clone()).is_err(), "slot {slot}");
            assert!(!directory.join("desktop.jsonl").exists());
            assert_eq!(std::fs::read(&target).unwrap(), b"archive\n");
            assert!(widened(&target));
        }
    }

    #[test]
    fn ticket_t_2_hardlink_archive_rotation_preserves_all_history_for_both_triggers() {
        for size in [false, true] {
            for slot in 1..=6 {
                let scratch = Scratch::new();
                let directory = scratch.0.join("logs");
                let logs = Logs::open(directory.clone()).unwrap();
                logs.event(Event::Starting, 30);
                let active = std::fs::read(directory.join("desktop.jsonl")).unwrap();
                for index in 1..=6 {
                    std::fs::write(
                        directory.join(format!("desktop.{index}.jsonl")),
                        format!("history {index}\n"),
                    )
                    .unwrap();
                }
                let target = scratch.0.join("outside");
                std::fs::hard_link(directory.join(format!("desktop.{slot}.jsonl")), &target)
                    .unwrap();
                widen(&target);
                force_rotation(&logs, size);
                for _ in 0..2 {
                    logs.event(Event::Ready, 30);
                }
                assert_eq!(
                    std::fs::read(directory.join("desktop.jsonl")).unwrap(),
                    active
                );
                for index in 1..=6 {
                    assert_eq!(
                        std::fs::read(directory.join(format!("desktop.{index}.jsonl"))).unwrap(),
                        format!("history {index}\n").as_bytes(),
                        "size={size}, linked slot={slot}, archive={index}"
                    );
                }
                assert_eq!(
                    std::fs::read(&target).unwrap(),
                    format!("history {slot}\n").as_bytes()
                );
                assert!(widened(&target));
            }
        }
    }

    #[test]
    fn ticket_t_2_owner_only_existing_modes_are_repaired_without_creating_archives() {
        let scratch = Scratch::new();
        let directory = scratch.0.join("logs");
        widen(&directory);
        for name in ["desktop.jsonl", "desktop.1.jsonl", "desktop.6.jsonl"] {
            let path = directory.join(name);
            std::fs::write(&path, b"history\n").unwrap();
            widen(&path);
        }
        let _logs = Logs::open(directory.clone()).unwrap();
        assert!(private(&directory));
        for name in ["desktop.jsonl", "desktop.1.jsonl", "desktop.6.jsonl"] {
            assert!(private(&directory.join(name)));
            assert_eq!(std::fs::read(directory.join(name)).unwrap(), b"history\n");
        }
        assert!(private(&directory.join("compat.log")));
        assert_eq!(std::fs::read_dir(directory).unwrap().count(), 4);
    }

    #[test]
    fn ticket_t_2_owner_only_modes_are_repaired_between_events_and_both_rotations() {
        for rotation in [None, Some(false), Some(true)] {
            let scratch = Scratch::new();
            let directory = scratch.0.join("logs");
            let path = directory.join("desktop.jsonl");
            let logs = Logs::open(directory.clone()).unwrap();
            logs.event(Event::Starting, 30);
            widen(&directory);
            widen(&path);
            if let Some(size) = rotation {
                for index in 1..=6 {
                    let archive = directory.join(format!("desktop.{index}.jsonl"));
                    std::fs::write(&archive, format!("history {index}\n")).unwrap();
                    widen(&archive);
                }
                force_rotation(&logs, size);
            }
            logs.event(Event::Ready, 30);
            assert!(private(&directory));
            assert!(private(&path));
            let bytes = std::fs::read(&path).unwrap();
            assert!(String::from_utf8(bytes).unwrap().contains("compat_ready"));
            if rotation.is_some() {
                for index in 1..=6 {
                    assert!(private(&directory.join(format!("desktop.{index}.jsonl"))));
                }
                assert!(
                    std::fs::read_to_string(directory.join("desktop.1.jsonl"))
                        .unwrap()
                        .contains("compat_starting")
                );
                for index in 2..=6 {
                    assert_eq!(
                        std::fs::read_to_string(directory.join(format!("desktop.{index}.jsonl")))
                            .unwrap(),
                        format!("history {}\n", index - 1)
                    );
                }
            }
        }
    }

    #[cfg(unix)]
    fn fifo(path: &Path) {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    }

    /// FIFOs are a Unix file type; Windows pipes do not live in directories.
    #[cfg(unix)]
    #[test]
    fn ticket_t_2_log_open_rejects_fifo_with_peer() {
        let scratch = Scratch::new();
        let path = scratch.0.join("logs/desktop.jsonl");
        fifo(&path);
        let _peer = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        assert!(Logs::open(scratch.0.join("logs")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn ticket_t_2_log_open_rejects_readerless_fifo_promptly() {
        const CHILD_PATH: &str = "PP_TEST_READERLESS_LOG_FIFO";
        if let Some(directory) = std::env::var_os(CHILD_PATH) {
            assert!(Logs::open(PathBuf::from(directory)).is_err());
            return;
        }
        let scratch = Scratch::new();
        fifo(&scratch.0.join("logs/desktop.jsonl"));
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "logs::tests::ticket_t_2_log_open_rejects_readerless_fifo_promptly",
                "--nocapture",
            ])
            .env(CHILD_PATH, scratch.0.join("logs"))
            .spawn()
            .unwrap();
        let started = std::time::Instant::now();
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "FIFO admission child failed: {status}");
                break;
            }
            if started.elapsed() >= std::time::Duration::from_secs(2) {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("readerless FIFO blocked log admission for 2 seconds");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn ticket_t_2_log_open_rejects_symlink_without_writing_target() {
        let scratch = Scratch::new();
        let target = scratch.0.join("outside");
        std::fs::write(&target, "unchanged\n").unwrap();
        symlink_file(&target, &scratch.0.join("logs/desktop.jsonl"));
        let result = Logs::open(scratch.0.join("logs"));
        if let Ok(logs) = &result {
            logs.event(Event::Ready, 30);
        }
        assert_eq!(std::fs::read_to_string(target).unwrap(), "unchanged\n");
        assert!(result.is_err());
    }

    #[test]
    fn ticket_t_2_log_open_rejects_symlinked_directory_without_writing_target() {
        let scratch = Scratch::new();
        let target = scratch.0.join("outside");
        std::fs::create_dir(&target).unwrap();
        std::fs::remove_dir(scratch.0.join("logs")).unwrap();
        symlink_dir(&target, &scratch.0.join("logs"));
        let result = Logs::open(scratch.0.join("logs"));
        if let Ok(logs) = &result {
            logs.event(Event::Ready, 30);
        }
        assert_eq!(std::fs::read_dir(target).unwrap().count(), 0);
        assert!(result.is_err());
    }

    #[test]
    fn ticket_t_2_log_rotation_stays_in_admitted_directory_after_symlink_swap() {
        let scratch = Scratch::new();
        let directory = scratch.0.join("logs");
        let logs = Logs::open(directory.clone()).unwrap();
        logs.event(Event::Starting, 30);
        let admitted = scratch.0.join("admitted");
        let target = scratch.0.join("outside");
        std::fs::create_dir(&target).unwrap();
        #[cfg(unix)]
        {
            std::fs::rename(&directory, &admitted).unwrap();
            symlink_dir(&target, &directory);
        }
        // Windows refuses to move the admitted directory while the logs hold it.
        #[cfg(windows)]
        let admitted = {
            assert!(std::fs::rename(&directory, &admitted).is_err());
            directory
        };
        logs.events.lock().unwrap().day = 0;
        logs.event(Event::Ready, 30);
        assert_eq!(std::fs::read_dir(target).unwrap().count(), 0);
        let row = |name: &str| -> serde_json::Value {
            serde_json::from_slice(&std::fs::read(admitted.join(name)).unwrap()).unwrap()
        };
        assert_eq!(row("desktop.1.jsonl")["event"], "compat_starting");
        assert_eq!(row("desktop.jsonl")["event"], "compat_ready");
    }

    #[test]
    fn ticket_t_2_log_rotation_rejects_dangling_symlink_replacement() {
        let scratch = Scratch::new();
        let path = scratch.0.join("logs/desktop.jsonl");
        let logs = Logs::open(scratch.0.join("logs")).unwrap();
        logs.event(Event::Starting, 30);
        let before = std::fs::read(&path).unwrap();
        let original = scratch.0.join("original");
        std::fs::rename(&path, &original).unwrap();
        let target = scratch.0.join("outside");
        symlink_file(&target, &path);
        logs.events.lock().unwrap().day = 0;
        logs.event(Event::Ready, 30);
        assert!(!target.exists(), "rotation created the symlink target");
        assert_eq!(std::fs::read(original).unwrap(), before);
        assert!(std::fs::symlink_metadata(path).unwrap().is_symlink());
    }

    #[test]
    fn ticket_t_2_log_rotation_preserves_archives_with_dangling_active_symlink() {
        for size_rollover in [false, true] {
            let scratch = Scratch::new();
            let directory = scratch.0.join("logs");
            let path = directory.join("desktop.jsonl");
            let logs = Logs::open(directory.clone()).unwrap();
            logs.event(Event::Starting, 30);
            let before = std::fs::read(&path).unwrap();
            let original = scratch.0.join("original");
            std::fs::rename(&path, &original).unwrap();
            let target = scratch.0.join("outside");
            symlink_file(&target, &path);
            let archives = ["one\n", "two\n", "three\n", "four\n", "five\n", "six\n"];
            for (index, bytes) in archives.iter().enumerate() {
                std::fs::write(
                    directory.join(format!("desktop.{}.jsonl", index + 1)),
                    bytes,
                )
                .unwrap();
            }
            {
                let mut log = logs.events.lock().unwrap();
                if size_rollover {
                    log.bytes = 10 * 1024 * 1024;
                } else {
                    log.day = 0;
                }
            }
            for _ in 0..3 {
                logs.event(Event::Ready, 30);
            }
            for (index, bytes) in archives.iter().enumerate() {
                assert_eq!(
                    std::fs::read(directory.join(format!("desktop.{}.jsonl", index + 1))).ok(),
                    Some(bytes.as_bytes().to_vec()),
                    "archive {} changed after repeated events, size_rollover={size_rollover}",
                    index + 1
                );
            }
            assert!(!target.exists(), "rotation created the symlink target");
            assert_eq!(std::fs::read(original).unwrap(), before);
            assert!(std::fs::symlink_metadata(path).unwrap().is_symlink());
        }
    }

    #[tokio::test]
    async fn ticket_t_2_log_appends_redacted_records_and_rotates_regular_files() {
        let scratch = Scratch::new();
        let path = scratch.0.join("logs/desktop.jsonl");
        let seed = b"{\"time\":1,\"level\":30,\"event\":\"compat_starting\"}\n";
        std::fs::write(&path, seed).unwrap();
        let logs = Logs::open(scratch.0.join("logs")).unwrap();
        logs.capture(&b"{\"level\":50,\"message\":\"SECRET_TOKEN\"}\n"[..])
            .await;
        let appended = std::fs::read(&path).unwrap();
        assert!(appended.starts_with(seed));
        let row: serde_json::Value = serde_json::from_slice(&appended[seed.len()..]).unwrap();
        assert_eq!(row["event"], "compat_output");
        assert_eq!(row["level"], 50);
        assert_eq!(row.as_object().unwrap().len(), 3);
        logs.events.lock().unwrap().day = 0;
        logs.event(Event::Ready, 30);
        assert_eq!(
            std::fs::read(scratch.0.join("logs/desktop.1.jsonl")).unwrap(),
            appended
        );
        let row: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(row["event"], "compat_ready");
        assert_eq!(row["level"], 30);
        assert_eq!(row.as_object().unwrap().len(), 3);
    }

    #[test]
    fn ticket_t_2_log_view_reprojects_legacy_rows_and_rejects_private_fields() {
        let scratch = Scratch::new();
        std::fs::write(scratch.0.join("logs/desktop.jsonl"), concat!(
            "{\"time\":1,\"level\":30,\"event\":\"compat_ready\",\"message\":\"SECRET_PRIVATE_PATH\"}\n",
            "{\"time\":2,\"level\":50,\"event\":\"SECRET_PRIVATE_EVENT\"}\n",
            "{\"time\":3,\"level\":99,\"event\":\"compat_output\"}\n",
            "{\"time\":4,\"level\":50,\"event\":\"compat_failure\",\"reason\":\"child_exit\",\"termination\":{\"kind\":\"signal\",\"value\":9,\"secret\":\"SECRET_TOKEN\"}}\n",
            "{\"time\":5,\"level\":50,\"event\":\"compat_failure\",\"reason\":\"SECRET_REASON\",\"termination\":null}\n",
        )).unwrap();
        assert_eq!(
            recent_diagnostics(&scratch.0).unwrap(),
            concat!(
                "Showing recent safe records. Older or invalid records were omitted.\n\n",
                "{\"time\":1,\"level\":30,\"event\":\"compat_ready\"}\n",
                "{\"time\":4,\"level\":50,\"event\":\"compat_failure\",\"reason\":\"child_exit\",\"termination\":{\"kind\":\"signal\",\"value\":9}}\n",
            )
        );
    }

    #[test]
    fn ticket_t_2_log_view_bounds_history_and_drops_partial_oversized_rows() {
        let scratch = Scratch::new();
        let mut file = File::create(scratch.0.join("logs/desktop.jsonl")).unwrap();
        file.write_all(&vec![b'x'; 70 * 1024]).unwrap();
        file.write_all(b"\n").unwrap();
        for time in 1..=110 {
            writeln!(
                file,
                "{{\"time\":{time},\"level\":30,\"event\":\"compat_ready\"}}"
            )
            .unwrap();
        }
        file.write_all(b"SECRET_PARTIAL").unwrap();
        let text = recent_diagnostics(&scratch.0).unwrap();
        let rows: Vec<serde_json::Value> = text
            .lines()
            .filter(|row| row.starts_with('{'))
            .map(|row| serde_json::from_str(row).unwrap())
            .collect();
        assert_eq!(rows.len(), 100);
        assert_eq!(rows[0]["time"], 11);
        assert_eq!(rows[99]["time"], 110);
        assert!(!text.contains("SECRET_PARTIAL"));
        assert!(text.len() < 20 * 1024);
    }

    #[test]
    fn ticket_t_2_log_view_rejects_symlinks_and_reads_rotation_chronologically() {
        let scratch = Scratch::new();
        let outside = scratch.0.join("outside");
        std::fs::write(&outside, "{\"time\":1,\"level\":30,\"event\":\"compat_output\",\"message\":\"SECRET_SYMLINK\"}\n").unwrap();
        symlink_file(&outside, &scratch.0.join("logs/desktop.6.jsonl"));
        std::fs::write(
            scratch.0.join("logs/desktop.1.jsonl"),
            "{\"time\":2,\"level\":30,\"event\":\"compat_starting\"}\n",
        )
        .unwrap();
        std::fs::write(
            scratch.0.join("logs/desktop.jsonl"),
            "{\"time\":3,\"level\":30,\"event\":\"compat_ready\"}\n",
        )
        .unwrap();
        let text = recent_diagnostics(&scratch.0).unwrap();
        assert_eq!(
            text,
            concat!(
                "Showing recent safe records. Older or invalid records were omitted.\n\n",
                "{\"time\":2,\"level\":30,\"event\":\"compat_starting\"}\n",
                "{\"time\":3,\"level\":30,\"event\":\"compat_ready\"}\n",
            )
        );
        let linked = scratch.0.join("linked-data");
        std::fs::create_dir(&linked).unwrap();
        symlink_dir(&scratch.0.join("logs"), &linked.join("logs"));
        assert_eq!(
            recent_diagnostics(&linked).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
    }

    #[tokio::test]
    async fn ticket_t_2_capture_redacts_secrets_even_without_a_final_newline() {
        let scratch = Scratch::new();
        let logs = Logs::open(scratch.0.join("logs")).unwrap();
        logs.capture(
            &b"{\"level\":50,\"event\":\"desktop_start_failed\",\"message\":\"SECRET_TOKEN\"}"[..],
        )
        .await;
        let text = recent_diagnostics(&scratch.0).unwrap();
        let row: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(row["event"], "compat_start_failed");
        assert_eq!(row["level"], 50);
        assert_eq!(row.as_object().unwrap().len(), 3);
        assert!(!text.contains("SECRET_TOKEN"));
    }

    #[test]
    fn gre_696_racing_first_log_opens_share_one_file() {
        let scratch = Scratch::new();
        let path = scratch.0.join("logs/desktop.jsonl");
        let directory = platform::open_directory(&scratch.0.join("logs")).unwrap();
        let threads = 8;
        for trial in 0..200 {
            let barrier = std::sync::Barrier::new(threads);
            let opened: Vec<_> = std::thread::scope(|scope| {
                let handles: Vec<_> = (0..threads)
                    .map(|_| {
                        scope.spawn(|| {
                            barrier.wait();
                            open_log(&directory, DESKTOP)
                                .map(|file| identity(&file))
                                .map_err(|error| error.to_string())
                        })
                    })
                    .collect();
                handles.into_iter().map(|h| h.join().unwrap()).collect()
            });
            let inode = path_identity(&path);
            assert!(
                opened.iter().all(|result| result == &Ok(inode)),
                "trial {trial}: {opened:?}"
            );
            std::fs::remove_file(&path).unwrap();
        }
    }

    #[test]
    fn gre_701_unlink_before_reopen_fails_closed_without_recreating() {
        let scratch = Scratch::new();
        let path = scratch.0.join("logs/desktop.jsonl");
        let directory = platform::open_directory(&scratch.0.join("logs")).unwrap();
        std::fs::write(&path, "").unwrap();
        let mut windows = Vec::new();
        let error = open_log_observed(&directory, DESKTOP, |window| {
            windows.push(window);
            if window == OpenWindow::Reopen {
                std::fs::remove_file(&path).unwrap();
            }
        })
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert_eq!(windows, [OpenWindow::Reopen]);
        assert!(std::fs::symlink_metadata(&path).is_err());
    }

    #[test]
    fn gre_701_unlink_before_secure_fails_closed_for_created_and_existing_logs() {
        for existing in [false, true] {
            let scratch = Scratch::new();
            let path = scratch.0.join("logs/desktop.jsonl");
            let directory = platform::open_directory(&scratch.0.join("logs")).unwrap();
            if existing {
                std::fs::write(&path, "").unwrap();
            }
            let mut windows = Vec::new();
            let error = open_log_observed(&directory, DESKTOP, |window| {
                windows.push(window);
                if window == OpenWindow::Secure {
                    std::fs::remove_file(&path).unwrap();
                }
            })
            .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
            let expected: &[_] = if existing {
                &[OpenWindow::Reopen, OpenWindow::Secure]
            } else {
                &[OpenWindow::Secure]
            };
            assert_eq!(windows, expected, "existing {existing}");
            assert!(std::fs::symlink_metadata(&path).is_err());
        }
    }

    const T13_SECRETS: [&str; 8] = [
        "PLANTED_BEARER_1f9c",
        "PLANTED_COOKIE_77aa",
        "PLANTED_BOOTSTRAP_e1",
        "PLANTED_PRINCIPAL_5d",
        "PLANTED_API_KEY_beef",
        "PLANTED_RUNTIME_KEY_ab12cd34",
        "PLANTED_PANIC_TOKEN",
        "PLANTED_SESSION_ID_77",
    ];

    fn assert_clean(text: &str) {
        for secret in T13_SECRETS {
            assert!(!text.contains(secret), "{secret} leaked: {text}");
        }
    }

    fn rows(path: &Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|row| serde_json::from_str(row).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn ticket_t_13_compat_capture_writes_scrubbed_pino_and_text_to_compat_log() {
        let scratch = Scratch::new();
        let logs = Logs::open(scratch.0.join("logs"))
            .unwrap()
            .with_secret("PLANTED_RUNTIME_KEY_ab12cd34");
        let output = concat!(
            "{\"level\":30,\"time\":1,\"msg\":\"request completed\",\"req\":{\"url\":\"/desktop?bootstrap=PLANTED_BOOTSTRAP_e1\",",
            "\"headers\":{\"authorization\":\"Bearer PLANTED_BEARER_1f9c\",\"cookie\":\"pp_session=PLANTED_COOKIE_77aa\",",
            "\"x-pp-principal\":\"PLANTED_PRINCIPAL_5d\",\"accept\":\"text/html\"}}}\n",
            "{\"level\":50,\"time\":2,\"msg\":\"spoolman sync failed\",\"err\":{\"type\":\"Error\",",
            "\"message\":\"GET http://spoolman.local/api?api_key=PLANTED_API_KEY_beef\",",
            "\"stack\":\"Error: key PLANTED_RUNTIME_KEY_ab12cd34\\n    at sync.js:4\"}}\n",
            "plain stderr Authorization: Bearer PLANTED_BEARER_1f9c\n",
            "{\"level\":40,\"time\":3,\"msg\":\"unterminated cookie: PLANTED_COOKIE_77aa\"}",
        );
        logs.capture(output.as_bytes()).await;
        let compat = std::fs::read_to_string(scratch.0.join("logs/compat.log")).unwrap();
        assert_clean(&compat);
        let rows = rows(&scratch.0.join("logs/compat.log"));
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0]["msg"], "request completed");
        assert_eq!(rows[0]["req"]["url"], "/desktop?bootstrap=[REDACTED]");
        assert_eq!(rows[0]["req"]["headers"]["authorization"], redact::REDACTED);
        assert_eq!(rows[0]["req"]["headers"]["accept"], "text/html");
        assert_eq!(rows[1]["level"], 50);
        assert_eq!(
            rows[1]["err"]["message"],
            "GET http://spoolman.local/api?api_key=[REDACTED]"
        );
        assert_eq!(rows[2]["level"], 30);
        assert_eq!(rows[2]["msg"], "plain stderr Authorization: [REDACTED]");
        assert_eq!(rows[3]["level"], 40);
        assert!(private(&scratch.0.join("logs/compat.log")));
        let events = std::fs::read_to_string(scratch.0.join("logs/desktop.jsonl")).unwrap();
        assert_eq!(events.lines().count(), 4);
        assert_clean(&events);
        assert_clean(&recent_diagnostics(&scratch.0).unwrap());
    }

    #[tokio::test]
    async fn ticket_t_13_capture_bounds_oversized_lines() {
        let scratch = Scratch::new();
        let logs = Logs::open(scratch.0.join("logs")).unwrap();
        let mut output = vec![b'a'; 1024 * 1024];
        output.extend_from_slice(b"\n{\"level\":30,\"msg\":\"after\"}\n");
        logs.capture(&output[..]).await;
        let rows = rows(&scratch.0.join("logs/compat.log"));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["msg"].as_str().unwrap().len(), LINE_BYTES);
        assert_eq!(rows[1]["msg"], "after");
    }

    #[test]
    fn ticket_t_13_core_log_records_scrubbed_tracing_events_and_error_chains() {
        let scratch = Scratch::new();
        let core = CoreLog::open(&scratch.0).unwrap();
        let error = anyhow::anyhow!("cookie: pp_session=PLANTED_COOKIE_77aa")
            .context("GET /desktop?bootstrap=PLANTED_BOOTSTRAP_e1")
            .context("Compatibility request failed");
        tracing::subscriber::with_default(core, || {
            tracing::debug!("not recorded below info");
            tracing::error!(
                error = AsRef::<dyn std::error::Error + 'static>::as_ref(&error),
                api_key = "PLANTED_API_KEY_beef",
                attempt = 3u64,
                "proxy rejected Authorization: Bearer PLANTED_BEARER_1f9c"
            );
            tracing::info!(target: "pp_core::startup", "core ready");
        });
        let text = std::fs::read_to_string(scratch.0.join("logs/core.log")).unwrap();
        assert_clean(&text);
        let rows = rows(&scratch.0.join("logs/core.log"));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["level"], 50);
        assert_eq!(rows[0]["target"], module_path!());
        assert_eq!(
            rows[0]["message"],
            "proxy rejected Authorization: [REDACTED]"
        );
        assert_eq!(
            rows[0]["error"],
            "Compatibility request failed: GET /desktop?bootstrap=[REDACTED]: cookie: [REDACTED]"
        );
        assert_eq!(rows[0]["api_key"], redact::REDACTED);
        assert_eq!(rows[0]["attempt"], 3);
        assert_eq!(rows[1]["level"], 30);
        assert_eq!(rows[1]["target"], "pp_core::startup");
        assert!(private(&scratch.0.join("logs/core.log")));
    }

    #[test]
    fn gre_1006_core_log_redacts_each_sensitive_field_kind() {
        let scratch = Scratch::new();
        let core = CoreLog::open(&scratch.0).unwrap();
        let headers = std::collections::BTreeMap::from([
            ("authorization", "token PLANTED_HDR_AUTH_01"),
            ("cookie", "pp_session=PLANTED_HDR_COOKIE_02"),
            ("x-api-key", "PLANTED_HDR_APIKEY_03"),
        ]);
        tracing::subscriber::with_default(core, || {
            tracing::warn!(
                password = "PLANTED_FIELD_PASSWORD",
                token = "PLANTED_FIELD_TOKEN",
                cookie = "PLANTED_FIELD_COOKIE",
                authorization = "PLANTED_FIELD_AUTHZ",
                webhook = "PLANTED_FIELD_WEBHOOK",
                api_key = "PLANTED_FIELD_API_KEY",
                github_pat = "PLANTED_FIELD_GITHUB_PAT",
                api_keys_v1 = "PLANTED_FIELD_API_KEYS_V1",
                discord_webhook_url = "PLANTED_FIELD_DISCORD_URL",
                "fields"
            );
            tracing::warn!(
                url = "https://discord.com/api/webhooks/1234/PLANTED_URL_WEBHOOK",
                "POST https://u:PLANTED_URL_PASSWORD@host/x?token=PLANTED_URL_TOKEN&api_key=PLANTED_URL_API_KEY"
            );
            tracing::warn!(?headers, "upstream request");
            tracing::warn!(
                "Cookie: a=PLANTED_LINE_COOKIE; Authorization: Bearer PLANTED_LINE_BEARER"
            );
        });
        let text = std::fs::read_to_string(scratch.0.join("logs/core.log")).unwrap();
        assert_eq!(text.matches("PLANTED_").count(), 0, "secret leaked: {text}");
        let rows = rows(&scratch.0.join("logs/core.log"));
        assert_eq!(rows.len(), 4);
        for name in [
            "password",
            "token",
            "cookie",
            "authorization",
            "webhook",
            "api_key",
            "github_pat",
            "api_keys_v1",
            "discord_webhook_url",
        ] {
            assert_eq!(rows[0][name], redact::REDACTED, "{name}");
        }
        assert_eq!(rows[0]["message"], "fields");
        assert_eq!(
            rows[1]["url"],
            "https://discord.com/api/webhooks/1234/[REDACTED]"
        );
        assert_eq!(
            rows[1]["message"],
            "POST https://[REDACTED]@host/x?token=[REDACTED]&api_key=[REDACTED]"
        );
        assert_eq!(
            rows[2]["headers"],
            r#"{"authorization": "[REDACTED]", "cookie": "[REDACTED]", "x-api-key": "[REDACTED]"}"#
        );
        assert_eq!(rows[3]["message"], "Cookie: [REDACTED]");
    }

    #[test]
    fn ticket_t_13_core_log_truncates_oversized_rows() {
        let scratch = Scratch::new();
        let core = CoreLog::open(&scratch.0).unwrap();
        let huge = "x".repeat(1024 * 1024);
        tracing::subscriber::with_default(core, || tracing::warn!("{huge}"));
        let rows = rows(&scratch.0.join("logs/core.log"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["truncated"], true);
        assert_eq!(rows[0]["message"].as_str().unwrap().len(), LINE_BYTES / 2);
        let scratch = Scratch::new();
        let core = CoreLog::open(&scratch.0).unwrap();
        let wide = "\u{20ac}".repeat(1024 * 1024);
        tracing::subscriber::with_default(core, || tracing::warn!("{wide}"));
        let text = std::fs::read_to_string(scratch.0.join("logs/core.log")).unwrap();
        assert!(text.len() <= LINE_BYTES);
        let row: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(row["truncated"], true);
    }

    fn stream_files(directory: &Path, stream: Stream) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.starts_with(&format!("{}.", stream.stem)))
            .collect();
        names.sort();
        names
    }

    #[tokio::test]
    async fn ticket_t_13_forced_rotation_keeps_seven_files_for_each_source() {
        let scratch = Scratch::new();
        let directory = scratch.0.join("logs");
        let logs = Logs::open(directory.clone()).unwrap();
        let core = CoreLog::open(&scratch.0).unwrap();
        for round in 0..10u64 {
            let size = round % 2 == 0;
            {
                let mut compat = logs.compat.lock().unwrap();
                let mut sink = core.sink.lock().unwrap();
                let Sink::File(core_file) = &mut *sink else {
                    panic!("core log not attached")
                };
                for log in [&mut *compat, core_file] {
                    if size {
                        log.bytes = FILE_BYTES;
                    } else {
                        log.day = 0;
                    }
                }
            }
            logs.capture(format!("{{\"level\":30,\"msg\":\"round {round}\"}}\n").as_bytes())
                .await;
            tracing::subscriber::with_default(core.clone(), || tracing::info!("round {round}"));
        }
        for (stream, field) in [(COMPAT, "msg"), (CORE, "message")] {
            let expected: Vec<String> = std::iter::once(format!("{}.log", stream.stem))
                .chain((1..=ARCHIVES).map(|index| format!("{}.{index}.log", stream.stem)))
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            assert_eq!(stream_files(&directory, stream), expected);
            assert_eq!(
                rows(&directory.join(format!("{}.log", stream.stem)))[0][field],
                "round 9"
            );
            for index in 1..=ARCHIVES {
                let archive = directory.join(format!("{}.{index}.log", stream.stem));
                assert_eq!(rows(&archive)[0][field], format!("round {}", 9 - index));
                assert!(private(&archive));
            }
        }
    }

    #[test]
    fn ticket_t_13_panic_writes_local_crash_record_without_telemetry() {
        let scratch = Scratch::new();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "logs::tests::core_log_panic_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("PP_T13_PANIC_DATA", &scratch.0)
            .env_remove("SENTRY_DSN")
            .output()
            .unwrap();
        assert!(!output.status.success());
        let text = std::fs::read_to_string(scratch.0.join("logs/core.log")).unwrap();
        assert_clean(&text);
        let rows = rows(&scratch.0.join("logs/core.log"));
        let panic = rows.iter().find(|row| row["target"] == "panic").unwrap();
        assert_eq!(panic["level"], 60);
        assert_eq!(panic["message"], "renderer bridge lost token=[REDACTED]");
        assert!(panic["location"].as_str().unwrap().contains("logs.rs:"));
        assert_eq!(rows[0]["message"], "before admission session_id=[REDACTED]");
        assert_eq!(rows[0]["level"], 40);
        assert_eq!(rows[1]["message"], "before crash");
    }

    #[test]
    fn ticket_t_13_core_log_holds_early_rows_until_attach_and_bounds_them() {
        let scratch = Scratch::new();
        let core = CoreLog::buffered();
        tracing::subscriber::with_default(core.clone(), || {
            tracing::warn!("cutover_evidence_unsaved");
            for index in 0..PENDING_ROWS + 10 {
                tracing::info!("early {index}");
            }
        });
        assert!(!scratch.0.join("logs/core.log").exists());
        core.attach(&scratch.0).unwrap();
        tracing::subscriber::with_default(core, || tracing::info!("attached"));
        let rows = rows(&scratch.0.join("logs/core.log"));
        assert_eq!(rows.len(), PENDING_ROWS + 2);
        assert_eq!(rows[0]["message"], "core log dropped 11 early rows");
        assert_eq!(rows[0]["level"], 40);
        assert_eq!(rows[1]["message"], "early 10");
        assert_eq!(
            rows[PENDING_ROWS]["message"],
            format!("early {}", PENDING_ROWS + 9)
        );
        assert_eq!(rows[PENDING_ROWS + 1]["message"], "attached");
    }

    #[test]
    #[ignore = "runs in an isolated subprocess and panics on purpose"]
    fn core_log_panic_child() {
        let data = PathBuf::from(std::env::var_os("PP_T13_PANIC_DATA").unwrap());
        let log = CoreLog::buffered();
        assert!(log.install());
        tracing::warn!("before admission session_id={}", "PLANTED_SESSION_ID_77");
        log.attach(&data).unwrap();
        tracing::info!("before crash");
        panic!("renderer bridge lost token={}", "PLANTED_PANIC_TOKEN");
    }

    #[tokio::test]
    async fn ticket_t_13_log_view_shows_compat_and_core_sources_scrubbed() {
        let scratch = Scratch::new();
        let logs = Logs::open(scratch.0.join("logs")).unwrap();
        // A row written before slice 1 scrubbed compat.log: the view scrubs again.
        std::fs::write(
            scratch.0.join("logs/compat.1.log"),
            "{\"level\":30,\"time\":1000,\"msg\":\"legacy Authorization: Bearer PLANTED_BEARER_1f9c\"}\n",
        )
        .unwrap();
        logs.capture(&b"{\"level\":40,\"time\":2000,\"msg\":\"sync slow\",\"cookie\":\"PLANTED_COOKIE_77aa\"}\n"[..])
            .await;
        let core = CoreLog::open(&scratch.0).unwrap();
        tracing::subscriber::with_default(core, || {
            tracing::error!(api_key = "PLANTED_API_KEY_beef", "printer bridge lost");
        });
        std::fs::OpenOptions::new()
            .append(true)
            .open(scratch.0.join("logs/core.log"))
            .unwrap()
            .write_all(b"{\"level\":99,\"message\":\"bad level\"}\nnot json\n")
            .unwrap();

        let text = recent_logs(&scratch.0).unwrap();
        assert_clean(&text);
        let events = text.find("Service events (desktop.jsonl)").unwrap();
        let compat = text.find("Compat backend (compat.log)").unwrap();
        let core = text.find("Core (core.log)").unwrap();
        assert!(events < compat && compat < core, "{text}");
        let section = |start: usize, end: usize| -> Vec<serde_json::Value> {
            text[start..end]
                .lines()
                .filter(|row| row.starts_with('{'))
                .map(|row| serde_json::from_str(row).unwrap())
                .collect()
        };
        let compat_rows = section(compat, core);
        assert_eq!(compat_rows.len(), 2);
        assert_eq!(compat_rows[0]["msg"], "legacy Authorization: [REDACTED]");
        assert_eq!(compat_rows[1]["msg"], "sync slow");
        assert_eq!(compat_rows[1]["cookie"], redact::REDACTED);
        let core_rows = section(core, text.len());
        assert_eq!(core_rows.len(), 1);
        assert_eq!(core_rows[0]["message"], "printer bridge lost");
        assert_eq!(core_rows[0]["api_key"], redact::REDACTED);
        assert!(text[core..].contains("Older or invalid records were omitted."));
        assert_eq!(section(events, compat).len(), 1);
    }

    #[test]
    fn ticket_t_13_log_view_skips_symlinked_sources_and_reports_empty_ones() {
        let scratch = Scratch::new();
        let outside = scratch.0.join("outside.log");
        std::fs::write(&outside, "{\"level\":30,\"msg\":\"SECRET_SYMLINK\"}\n").unwrap();
        symlink_file(&outside, &scratch.0.join("logs/compat.log"));
        let text = recent_logs(&scratch.0).unwrap();
        assert!(!text.contains("SECRET_SYMLINK"), "{text}");
        let compat = text.find("Compat backend (compat.log)").unwrap();
        let core = text.find("Core (core.log)").unwrap();
        assert!(text[compat..core].contains("Older or invalid records were omitted."));
        assert!(text[core..].contains("No records yet."));
        let linked = scratch.0.join("linked-data");
        std::fs::create_dir(&linked).unwrap();
        symlink_dir(&scratch.0.join("logs"), &linked.join("logs"));
        assert_eq!(
            recent_logs(&linked).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn ticket_t_13_log_folder_admits_only_the_owned_logs_directory() {
        let scratch = Scratch::new();
        restrict(&scratch.0.join("logs"));
        assert_eq!(log_folder(&scratch.0).unwrap(), scratch.0.join("logs"));

        let linked = scratch.0.join("linked-data");
        std::fs::create_dir(&linked).unwrap();
        symlink_dir(&scratch.0.join("logs"), &linked.join("logs"));
        assert_eq!(
            log_folder(&linked).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );

        let file = scratch.0.join("file-data");
        std::fs::create_dir(&file).unwrap();
        std::fs::write(file.join("logs"), "").unwrap();
        assert_eq!(
            log_folder(&file).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );

        let missing = scratch.0.join("missing-data");
        assert_eq!(
            log_folder(&missing).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
        // A relative path could reach the opener as an option such as `-R`.
        assert_eq!(
            log_folder(Path::new("-data")).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
        // Group or world access means another account could plant files.
        widen(&scratch.0.join("logs"));
        assert_eq!(
            log_folder(&scratch.0).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
    }
}
