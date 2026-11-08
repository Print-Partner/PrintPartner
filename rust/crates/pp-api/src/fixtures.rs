//! Isolated Node/Rust parity runner for copied fixtures (T-10-fixtures).
//!
//! Each case replays a recorded Node exchange against a Rust router that owns a
//! disposable data directory and has no outbound providers. [`Ledger`] compares
//! the complete public response and keeps every deliberate Rust correction
//! explicit: a declared correction must still differ from Node, must match its
//! literal Rust response and must be used exactly once. [`StateGraph`] records
//! every database row and every other file under the data directory, so a test
//! can check database and file changes as well as responses.
use anyhow::{Context, Result};
use rusqlite::{OpenFlags, types::ValueRef};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::{Path, PathBuf},
    time::Duration,
};

/// Disposable data directory, removed on drop.
pub struct Sandbox(PathBuf);

impl Sandbox {
    pub fn new(label: &str) -> Result<Self> {
        let mut random = [0; 12];
        getrandom::fill(&mut random).map_err(|error| anyhow::anyhow!("{error}"))?;
        let path = std::env::temp_dir().join(format!("pp-parity-{label}-{}", hex::encode(random)));
        // WriterOwner refuses group/world-writable data directories, so do not
        // let a permissive umask decide the mode.
        private_dir_builder()
            .create(&path)
            .with_context(|| format!("create {}", path.display()))?;
        Ok(Self(path))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        // A failed removal must not replace the panic that is unwinding.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Creates directories with mode 0700, so a permissive umask cannot decide it.
#[cfg(unix)]
fn private_dir_builder() -> std::fs::DirBuilder {
    use std::os::unix::fs::DirBuilderExt;
    let mut builder = std::fs::DirBuilder::new();
    builder.mode(0o700);
    builder
}

/// Windows has no mode bits; the directory inherits its parent's ACL.
#[cfg(windows)]
fn private_dir_builder() -> std::fs::DirBuilder {
    std::fs::DirBuilder::new()
}

/// The public part of one HTTP response: status, complete JSON body and every
/// literal `Set-Cookie` header. An empty body is `null`. A Rust problem body is
/// compared without its RFC 9457 envelope (`type`, `title`, `status`), which
/// Node never sends; `crate::problem` tests check the envelope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Exchange {
    pub status: u16,
    pub body: Value,
    pub set_cookie: Vec<String>,
}

impl Exchange {
    pub async fn observe(response: reqwest::Response) -> Result<Self> {
        let status = response.status().as_u16();
        let set_cookie = response
            .headers()
            .get_all("set-cookie")
            .iter()
            .map(|value| value.to_str().map(str::to_owned))
            .collect::<Result<_, _>>()?;
        let bytes = response.bytes().await?;
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            crate::problem::without_envelope(serde_json::from_slice(&bytes)?)
        };
        Ok(Self {
            status,
            body,
            set_cookie,
        })
    }
}

/// A router served on an ephemeral IPv4 loopback listener, with a client that
/// uses no proxy and follows no redirects. `build` receives the listener origin.
pub struct Served {
    pub origin: String,
    pub client: reqwest::Client,
    serving: crate::serving::Serving,
}

impl Served {
    pub async fn start(build: impl FnOnce(&str) -> axum::Router) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let router = build(&origin);
        let serving = crate::serving::Serving::start(listener, router);
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            origin,
            client,
            serving,
        })
    }

    pub async fn stop(mut self) -> Result<()> {
        self.serving.stop().await
    }
}

/// Every row of every table in each SQLite database under a data directory,
/// plus the SHA-256 of every other file. Rows are JSON arrays in column order
/// (blobs, and text that is not valid UTF-8, hex-encoded),
/// sorted, so equal graphs compare equal regardless of physical row order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateGraph {
    tables: BTreeMap<String, Vec<String>>,
    files: BTreeMap<String, String>,
}

impl StateGraph {
    pub fn capture(root: &Path) -> Result<Self> {
        let mut graph = Self {
            tables: BTreeMap::new(),
            files: BTreeMap::new(),
        };
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(&directory)? {
                let entry = entry?;
                let path = entry.path();
                let name = path
                    .strip_prefix(root)?
                    .to_str()
                    .context("non-UTF-8 path")?
                    .replace('\\', "/");
                if entry.file_type()?.is_dir() {
                    pending.push(path);
                } else if name.ends_with(".db") {
                    graph.read_database(&name, &path)?;
                } else if !["-wal", "-shm", "-journal"]
                    .iter()
                    .any(|suffix| name.ends_with(&format!(".db{suffix}")))
                {
                    let digest = Sha256::digest(std::fs::read(&path)?);
                    graph.files.insert(name, hex::encode(digest));
                }
            }
        }
        Ok(graph)
    }

    fn read_database(&mut self, name: &str, path: &Path) -> Result<()> {
        let db = rusqlite::Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        db.busy_timeout(Duration::from_secs(5))?;
        let tables = db
            .prepare(
                "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for table in tables {
            let mut statement =
                db.prepare(&format!("SELECT * FROM \"{}\"", table.replace('"', "\"\"")))?;
            let width = statement.column_count();
            let mut rows = statement
                .query_map([], |row| {
                    (0..width)
                        .map(|index| {
                            Ok(match row.get_ref(index)? {
                                ValueRef::Null => Value::Null,
                                ValueRef::Integer(value) => json!(value),
                                ValueRef::Real(value) => json!(value),
                                ValueRef::Text(value) => match std::str::from_utf8(value) {
                                    Ok(text) => json!(text),
                                    Err(_) => json!({ "text": hex::encode(value) }),
                                },
                                ValueRef::Blob(value) => json!({ "blob": hex::encode(value) }),
                            })
                        })
                        .collect::<rusqlite::Result<Vec<_>>>()
                        .map(|cells| Value::Array(cells).to_string())
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.sort();
            self.tables.insert(format!("{name}/{table}"), rows);
        }
        Ok(())
    }

    /// Rows in `table`, named `<database file>/<table>`; `None` if it is absent.
    pub fn rows(&self, table: &str) -> Option<usize> {
        self.tables.get(table).map(Vec::len)
    }

    /// Names of the tables and files that differ between `self` and `after`,
    /// sorted; a table or file that exists on only one side is included.
    pub fn changed(&self, after: &Self) -> Vec<String> {
        fn differing<V: PartialEq>(
            before: &BTreeMap<String, V>,
            after: &BTreeMap<String, V>,
        ) -> BTreeSet<String> {
            before
                .keys()
                .chain(after.keys())
                .filter(|key| before.get(*key) != after.get(*key))
                .cloned()
                .collect()
        }
        let mut changed = differing(&self.tables, &after.tables);
        changed.extend(differing(&self.files, &after.files));
        changed.into_iter().collect()
    }
}

/// Why Rust deliberately answers differently from Node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CorrectionKind {
    Security,
    WebSocket,
    /// A documented difference of the durable store, such as a field Node's
    /// in-memory state never had or a failure text Node does not persist.
    Durable,
    /// A documented refusal of input Node accepts and saves.
    Validation,
}

/// One declared, literal Rust response that replaces Node's for a named case.
#[derive(Clone, Debug)]
pub struct Correction {
    pub case: &'static str,
    pub kind: CorrectionKind,
    pub reason: &'static str,
    pub rust: Exchange,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParityError {
    DuplicateCorrection(String),
    DuplicateCase(String),
    Mismatch {
        case: String,
        expected: Box<Exchange>,
        actual: Box<Exchange>,
    },
    StaleCorrection(String),
    UnusedCorrections(Vec<String>),
    NoCases,
}

impl fmt::Display for ParityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateCorrection(case) => write!(f, "{case}: correction declared twice"),
            Self::DuplicateCase(case) => write!(f, "{case}: case compared twice"),
            Self::Mismatch {
                case,
                expected,
                actual,
            } => write!(f, "{case}: expected {expected:?}, received {actual:?}"),
            Self::StaleCorrection(case) => {
                write!(f, "{case}: Rust now matches Node; remove the correction")
            }
            Self::UnusedCorrections(cases) => {
                write!(f, "corrections never compared: {}", cases.join(", "))
            }
            Self::NoCases => write!(f, "no cases were compared"),
        }
    }
}

impl std::error::Error for ParityError {}

/// Counts of one finished run. `corrected` lists each applied correction.
#[derive(Debug, PartialEq, Eq)]
pub struct Report {
    pub matched: usize,
    pub corrected: Vec<(String, CorrectionKind)>,
}

/// Response comparison for one fixture corpus.
pub struct Ledger {
    corrections: BTreeMap<&'static str, Correction>,
    seen: BTreeSet<String>,
    matched: usize,
    corrected: Vec<(String, CorrectionKind)>,
}

impl Ledger {
    pub fn new(corrections: Vec<Correction>) -> Result<Self, ParityError> {
        let mut declared = BTreeMap::new();
        for correction in corrections {
            let case = correction.case;
            if declared.insert(case, correction).is_some() {
                return Err(ParityError::DuplicateCorrection(case.into()));
            }
        }
        Ok(Self {
            corrections: declared,
            seen: BTreeSet::new(),
            matched: 0,
            corrected: Vec::new(),
        })
    }

    /// Requires `rust` to equal Node's response, or, for a case with a declared
    /// correction, to differ from Node and equal the correction exactly.
    pub fn compare(
        &mut self,
        case: &str,
        node: &Exchange,
        rust: &Exchange,
    ) -> Result<(), ParityError> {
        if !self.seen.insert(case.into()) {
            return Err(ParityError::DuplicateCase(case.into()));
        }
        let mismatch = |expected: &Exchange| ParityError::Mismatch {
            case: case.into(),
            expected: Box::new(expected.clone()),
            actual: Box::new(rust.clone()),
        };
        match self.corrections.get(case) {
            Some(_) if rust == node => Err(ParityError::StaleCorrection(case.into())),
            Some(correction) if *rust != correction.rust => Err(mismatch(&correction.rust)),
            Some(correction) => {
                self.corrected.push((case.into(), correction.kind));
                Ok(())
            }
            None if rust != node => Err(mismatch(node)),
            None => {
                self.matched += 1;
                Ok(())
            }
        }
    }

    /// Fails if nothing was compared or a declared correction was never used.
    pub fn finish(self) -> Result<Report, ParityError> {
        if self.seen.is_empty() {
            return Err(ParityError::NoCases);
        }
        let unused: Vec<String> = self
            .corrections
            .keys()
            .filter(|case| !self.seen.contains(**case))
            .map(|case| (*case).to_owned())
            .collect();
        if !unused.is_empty() {
            return Err(ParityError::UnusedCorrections(unused));
        }
        Ok(Report {
            matched: self.matched,
            corrected: self.corrected,
        })
    }
}

/// Replaces volatile values (random ids, clock readings) with labels numbered
/// in first-seen order across one capture, the way a Node generator labels its
/// golden. Each rule maps object keys to a label prefix; a string under one of
/// those keys becomes `<prefix>-<n>`, and equal strings keep equal labels.
pub struct Labels {
    rules: Vec<(&'static str, &'static [&'static str])>,
    seen: BTreeMap<(&'static str, String), String>,
}

impl Labels {
    pub fn new(rules: Vec<(&'static str, &'static [&'static str])>) -> Self {
        Self {
            rules,
            seen: BTreeMap::new(),
        }
    }

    /// The label for `raw` under `prefix`, assigning the next number if new.
    pub fn label(&mut self, prefix: &'static str, raw: &str) -> String {
        let next = self.seen.keys().filter(|(p, _)| *p == prefix).count() + 1;
        self.seen
            .entry((prefix, raw.to_owned()))
            .or_insert_with(|| format!("{prefix}-{next}"))
            .clone()
    }

    /// Labels every matching string in `exchange.body`, depth first in key order.
    pub fn scrub(&mut self, exchange: &mut Exchange) {
        self.scrub_value(&mut exchange.body);
    }

    fn scrub_value(&mut self, value: &mut Value) {
        match value {
            Value::Array(items) => items.iter_mut().for_each(|item| self.scrub_value(item)),
            Value::Object(fields) => {
                for (key, field) in fields.iter_mut() {
                    let prefix = self
                        .rules
                        .iter()
                        .find(|(_, keys)| keys.contains(&key.as_str()))
                        .map(|(prefix, _)| *prefix);
                    match (prefix, &*field) {
                        (Some(prefix), Value::String(raw)) => {
                            *field = Value::String(self.label(prefix, raw));
                        }
                        _ => self.scrub_value(field),
                    }
                }
            }
            _ => {}
        }
    }
}
