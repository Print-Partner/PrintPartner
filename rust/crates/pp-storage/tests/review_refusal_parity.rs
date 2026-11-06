#![cfg(feature = "desktop_rebuild")]

use pp_storage::{Limits, WriterOwner};
use rusqlite::{Connection, types::ValueRef};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

#[path = "common/process.rs"]
mod child_process;

const NODE: &str = include_str!("../../../../fixtures/parity/review-refusals/node.json");

#[derive(Debug, PartialEq)]
enum Refusal {
    /// `AcceptedRead::IntegrityFailure`, carried per Build in the batch.
    Integrity { code: String, message: String },
    /// Any other error, which fails the whole batch.
    Unexpected { stderr: String },
}
impl Refusal {
    fn class(&self) -> &'static str {
        match self {
            Self::Integrity { .. } => "integrity_failure",
            Self::Unexpected { .. } => "unexpected",
        }
    }
}

fn integrity(code: &str, message: &str) -> Refusal {
    Refusal::Integrity {
        code: code.into(),
        message: message.into(),
    }
}

/// Rust's typed carrier for each Node case. Codes differ from Node where the
/// Rust reader decodes PPJS before it compares the projection or digest.
/// The renamed-column control is a class divergence: Node throws a raw
/// SqliteError, while Rust reads the row without the column and fails the digest.
fn rust_expected(case: &str) -> Refusal {
    match case {
        "malformed-parts-ppjs" | "malformed-plan-revision-parts-ppjs" => {
            integrity("manifest_text", "Invalid PPJS manifest text header")
        }
        "non-scalar-metadata-stale-digest" => integrity(
            "revision_digest",
            "Accepted Plan revision digest is corrupt",
        ),
        "non-scalar-projection-mismatch" => integrity(
            "projection",
            "Accepted Plan projection differs from its revision",
        ),
        "unrelated-renamed-column" => integrity(
            "revision_digest",
            "Accepted Plan revision digest is corrupt",
        ),
        other => panic!("no Rust expectation for {other}"),
    }
}

struct Root(PathBuf);
impl Drop for Root {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn setup(fixture: &Value, sql: &str) -> Root {
    let root =
        Root(std::env::temp_dir().join(format!("pp-review-refusal-{}", rand::random::<u64>())));
    let sentinel = root.0.join(fixture["sentinel"]["path"].as_str().unwrap());
    std::fs::create_dir_all(sentinel.parent().unwrap()).unwrap();
    std::fs::write(&sentinel, fixture["sentinel"]["contents"].as_str().unwrap()).unwrap();
    std::fs::write(
        root.0.join("print-partner.db"),
        include_bytes!("fixtures/accepted-plan-node.db"),
    )
    .unwrap();
    let (owner, _) = WriterOwner::open(&root.0, Limits::default()).unwrap();
    owner.shutdown().unwrap();
    let connection = Connection::open(root.0.join("print-partner.db")).unwrap();
    let triggers = connection
        .prepare("SELECT name,sql FROM sqlite_master WHERE type='trigger'")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    for (name, _) in &triggers {
        connection
            .execute_batch(&format!("DROP TRIGGER \"{name}\";"))
            .unwrap();
    }
    connection.execute_batch(sql).unwrap();
    for (_, trigger) in triggers {
        if let Err(error) = connection.execute_batch(&trigger) {
            assert!(error.to_string().contains("no such table"), "{error}");
        }
    }
    root
}

fn rows(root: &Path, query: &str) -> Value {
    let connection = Connection::open(root.join("print-partner.db")).unwrap();
    let mut statement = match connection.prepare(query) {
        Ok(statement) => statement,
        Err(error) => {
            let message = match &error {
                rusqlite::Error::SqlInputError { msg, .. } => msg.clone(),
                rusqlite::Error::SqliteFailure(_, Some(msg)) => msg.clone(),
                other => other.to_string(),
            };
            return json!({ "error": message });
        }
    };
    let width = statement.column_count();
    let rows = statement
        .query_map([], |row| {
            (0..width)
                .map(|i| {
                    Ok(match row.get_ref(i)? {
                        ValueRef::Null => Value::Null,
                        ValueRef::Integer(n) => json!(n),
                        ValueRef::Text(s) => json!(std::str::from_utf8(s).unwrap()),
                        value => panic!("unexpected {value:?}"),
                    })
                })
                .collect::<rusqlite::Result<Vec<_>>>()
                .map(Value::Array)
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    Value::Array(rows)
}

/// Every row of every table, so a write to any column fails the preservation check.
fn database(root: &Path, cell: &str) -> Vec<String> {
    let connection = Connection::open(root.join("print-partner.db")).unwrap();
    let tables = connection
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let mut lines = Vec::new();
    for table in tables {
        let columns = connection
            .prepare("SELECT name FROM pragma_table_info(?1)")
            .unwrap()
            .query_map([&table], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|name| cell.replace("{column}", &name.unwrap()))
            .collect::<Vec<_>>();
        let mut statement = connection
            .prepare(&format!(
                "SELECT {} AS line FROM \"{table}\" ORDER BY line",
                columns.join("||','||")
            ))
            .unwrap();
        for line in statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
        {
            lines.push(format!("{table}:{}", line.unwrap()));
        }
    }
    lines
}

fn files(root: &Path) -> Value {
    let mut found = Vec::new();
    let mut pending = vec![root.join("repos")];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                // Node's fixture spells paths with '/' on every OS.
                let relative = path
                    .strip_prefix(root)
                    .unwrap()
                    .components()
                    .map(|part| part.as_os_str().to_str().unwrap())
                    .collect::<Vec<_>>()
                    .join("/");
                found.push(json!([relative, std::fs::read_to_string(&path).unwrap()]));
            }
        }
    }
    found.sort_by_key(|entry| entry[0].as_str().unwrap().to_owned());
    Value::Array(found)
}

fn review(root: &Path, session: &str) -> Refusal {
    let observations = root.join("observations.json");
    std::fs::write(
        &observations,
        r#"{"availableInputRoots":[1],"mediaByPartId":{"1":{"artifactMissing":false,"thumbEmpty":false},"2":{"artifactMissing":false,"thumbEmpty":false}}}"#,
    )
    .unwrap();
    let output = child_process::command(child_process::bin_exe("plan-read-fixture"))
        .arg(root)
        .arg("1")
        .env("PP_READ_SESSION", session)
        .env("PP_DESKTOP_REBUILD", "1")
        .env("PP_REVIEW_OBSERVATIONS", &observations)
        .output()
        .unwrap();
    if !output.status.success() {
        return Refusal::Unexpected {
            stderr: String::from_utf8(output.stderr).unwrap(),
        };
    }
    let output: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output["readOnly"]["unchanged"], true);
    let build = &output["builds"][0];
    assert_eq!(
        build["accepted"], build["review"],
        "accepted and Review refusals agree"
    );
    assert_eq!(build["review"], build["reviewAll"]);
    assert_eq!(build["review"]["kind"], "integrity_failure", "{build}");
    Refusal::Integrity {
        code: build["review"]["code"].as_str().unwrap().into(),
        message: build["review"]["message"].as_str().unwrap().into(),
    }
}

#[test]
fn review_refusals_match_node_class_and_preserve_stored_inputs() {
    let fixture: Value = serde_json::from_str(NODE).unwrap();
    let query = fixture["rowsQuery"].as_str().unwrap();
    let cell = fixture["cell"].as_str().unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 5);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let root = setup(&fixture, case["sql"].as_str().unwrap());
        assert_eq!(
            rows(&root.0, query),
            case["preserved"]["rows"],
            "{name} stored input"
        );
        assert_eq!(files(&root.0), case["preserved"]["files"], "{name} files");
        let database_before = database(&root.0, cell);
        let refusal = review(&root.0, "read-fixture-secret");
        assert_eq!(
            database(&root.0, cell),
            database_before,
            "{name} preserved database"
        );
        let expected = rust_expected(name);
        assert_eq!(refusal, expected, "{name}");
        if name == "unrelated-renamed-column" {
            assert_eq!(case["node"]["routeClass"], "unexpected");
            assert_eq!(case["node"]["thrown"]["name"], "SqliteError");
        } else {
            assert_eq!(refusal.class(), case["node"]["routeClass"], "{name} class");
        }
        assert_eq!(
            rows(&root.0, query),
            case["preserved"]["rows"],
            "{name} preserved rows"
        );
        assert_eq!(
            files(&root.0),
            case["preserved"]["files"],
            "{name} preserved files"
        );
    }
}

#[test]
fn unrelated_generic_error_does_not_satisfy_an_integrity_refusal() {
    let fixture: Value = serde_json::from_str(NODE).unwrap();
    let cell = fixture["cell"].as_str().unwrap();
    let malformed = &fixture["cases"][0];
    assert_eq!(malformed["name"], "malformed-parts-ppjs");
    let root = setup(&fixture, malformed["sql"].as_str().unwrap());
    let database_before = database(&root.0, cell);
    let refusal = review(&root.0, "not-the-read-session");
    assert_eq!(database(&root.0, cell), database_before);
    let Refusal::Unexpected { stderr } = &refusal else {
        panic!("{refusal:?}");
    };
    assert_eq!(stderr.trim_end(), "Error: Authentication required");
    for case in fixture["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        assert_ne!(refusal, rust_expected(name), "{name}");
    }
    assert_eq!(
        rows(&root.0, fixture["rowsQuery"].as_str().unwrap()),
        malformed["preserved"]["rows"]
    );
}
