//! Replays `fixtures/parity/github-sync/node.json` (GRE-429) against
//! `pp_services::sync`. Every case must produce Node's JSON and GitHub reads.

use pp_services::sync::{
    self, GitSourceRow, GithubPort, PortError, RawTree, RemoteUpdateStatus, SyncError, SyncLimits,
};
use serde_json::{Map, Value, json};
use std::sync::Mutex;

fn golden() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../fixtures/parity/github-sync/node.json"
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// The GitHub stub the generator served, with the same read log.
struct World {
    repos: Map<String, Value>,
    calls: Mutex<Vec<Vec<String>>>,
}

impl World {
    fn new(golden: &Value) -> Self {
        Self {
            repos: golden["world"].as_object().unwrap().clone(),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn take_calls(&self) -> Value {
        json!(std::mem::take(&mut *self.calls.lock().unwrap()))
    }

    fn read(&self, call: &[&str]) -> Result<&Value, PortError> {
        self.calls
            .lock()
            .unwrap()
            .push(call.iter().map(|part| part.to_string()).collect());
        let fail = |status| PortError {
            status: Some(status),
            message: format!("stub status {status}"),
        };
        let repo = self
            .repos
            .get(&format!("{}/{}", call[1], call[2]))
            .ok_or_else(|| fail(404))?;
        if let Some(status) = repo["fail"][call[0]].as_u64() {
            return Err(fail(status as u16));
        }
        Ok(repo)
    }

    fn names(&self, call: &[&str], key: &str) -> Result<Vec<String>, PortError> {
        let repo = self.read(call)?;
        Ok(repo[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|name| name.as_str().unwrap().to_owned())
            .collect())
    }
}

impl GithubPort for World {
    async fn default_branch(&self, owner: &str, repo: &str) -> Result<Option<String>, PortError> {
        let repo = self.read(&["repo", owner, repo])?;
        Ok(repo["default_branch"].as_str().map(str::to_owned))
    }

    async fn list_branches(&self, owner: &str, repo: &str) -> Result<Vec<String>, PortError> {
        self.names(&["branches", owner, repo], "branches")
    }

    async fn list_tags(&self, owner: &str, repo: &str) -> Result<Vec<String>, PortError> {
        self.names(&["tags", owner, repo], "tags")
    }

    async fn commit_sha(
        &self,
        owner: &str,
        repo: &str,
        git_ref: &str,
    ) -> Result<String, PortError> {
        let repo = self.read(&["commit", owner, repo, git_ref])?;
        repo["commits"][git_ref]
            .as_str()
            .map(str::to_owned)
            .ok_or(PortError {
                status: Some(422),
                message: "No commit found".into(),
            })
    }

    async fn tree(&self, owner: &str, repo: &str, tree_sha: &str) -> Result<RawTree, PortError> {
        let repo = self.read(&["tree", owner, repo, tree_sha])?;
        let tree = repo["trees"].get(tree_sha).ok_or(PortError {
            status: Some(404),
            message: "Not Found".into(),
        })?;
        Ok(serde_json::from_value(tree.clone()).unwrap())
    }
}

fn failure(error: SyncError) -> Value {
    match error {
        SyncError::Refused(message) => json!({ "message": message }),
        SyncError::Port(error) => json!({ "status": error.status }),
    }
}

fn outcome<T: serde::Serialize>(key: &str, result: Result<T, SyncError>) -> Map<String, Value> {
    let mut map = Map::new();
    match result {
        Ok(value) => map.insert(key.into(), serde_json::to_value(value).unwrap()),
        Err(error) => map.insert("error".into(), failure(error)),
    };
    map
}

/// Requires every Rust case to equal Node's and returns the count.
fn ledger(section: &str, node: &[Value], rust: &[Value]) -> usize {
    assert_eq!(node.len(), rust.len(), "{section}: case count");
    for (index, (node, rust)) in node.iter().zip(rust).enumerate() {
        let id = node["input"]["id"]
            .as_str()
            .map_or_else(|| index.to_string(), str::to_owned);
        assert_eq!(rust, node, "{section}/{id}: Rust differs from Node");
    }
    println!("{section}: {} exact", node.len());
    node.len()
}

fn opt_str(value: &Value) -> Option<&str> {
    value.as_str()
}

#[test]
fn ticket_t_59_sync_github_urls_match_node() {
    let golden = golden();
    let node = golden["urls"].as_array().unwrap();
    let rust = node
        .iter()
        .map(|case| {
            let input = &case["input"];
            let url = input["url"].as_str().unwrap();
            let requested = opt_str(&input["requested"]);
            let known = input["known"]
                .as_array()
                .map(|known| {
                    known
                        .iter()
                        .map(|value| value.as_str().unwrap())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            json!({
                "input": input,
                "parsed": sync::parse_github_url(url),
                "ref_name": sync::github_ref_name(url, requested),
                "location": sync::normalize_github_source_location(url, requested),
                "resolved": sync::resolve_github_url_ref(url, &known),
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(ledger("urls", node, &rust), 33);
}

#[test]
fn ticket_t_59_sync_repos_txt_matches_node() {
    let golden = golden();
    let node = golden["repos_lines"].as_array().unwrap();
    let rust = node
        .iter()
        .map(|case| {
            let line = case["line"].as_str().unwrap();
            json!({ "line": line, "parsed": sync::parse_repos_txt_line(line) })
        })
        .collect::<Vec<_>>();
    assert_eq!(ledger("repos_lines", node, &rust), 16);

    let node = golden["repos_texts"].as_array().unwrap();
    let rust = node
        .iter()
        .map(|case| {
            let text = case["text"].as_str().unwrap();
            let skipped = sync::repos_txt_skipped_names(text);
            json!({
                "text": text,
                "parsed": sync::parse_repos_txt_text(text),
                "skipped": skipped.len(),
                "skipped_names": skipped,
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(ledger("repos_texts", node, &rust), 3);
}

#[tokio::test]
async fn ticket_t_59_sync_branch_and_tag_listings_match_node() {
    let golden = golden();
    let world = World::new(&golden);
    for (section, tags) in [("branches", false), ("tags", true)] {
        let node = golden[section].as_array().unwrap();
        let mut rust = Vec::new();
        for case in node {
            let url = case["url"].as_str().unwrap();
            let mut map = if tags {
                outcome("ok", sync::list_tags(&world, url).await)
            } else {
                outcome("ok", sync::list_branches(&world, url).await)
            };
            map.insert("url".into(), json!(url));
            map.insert("calls".into(), world.take_calls());
            rust.push(Value::Object(map));
        }
        assert_eq!(ledger(section, node, &rust), 7);
    }
}

fn limits(options: &Value) -> SyncLimits {
    let defaults = SyncLimits::default();
    SyncLimits {
        max_stl_files: options["maxStlFiles"]
            .as_u64()
            .unwrap_or(defaults.max_stl_files),
        max_documentation_bytes: options["maxDocsBytes"]
            .as_u64()
            .unwrap_or(defaults.max_documentation_bytes),
        file_timeout_ms: options["fileTimeoutMs"]
            .as_u64()
            .unwrap_or(defaults.file_timeout_ms),
    }
}

#[tokio::test]
async fn ticket_t_59_sync_snapshot_plans_match_node() {
    let golden = golden();
    let world = World::new(&golden);
    let node = golden["syncs"].as_array().unwrap();
    let mut rust = Vec::new();
    for case in node {
        let input = &case["input"];
        let result = sync::plan_github_sync(
            &world,
            input["url"].as_str().unwrap(),
            input["branch"].as_str().unwrap(),
            opt_str(&input["tag"]),
            limits(&input["options"]),
        )
        .await;
        let mut map = outcome("plan", result);
        map.insert("input".into(), input.clone());
        map.insert("calls".into(), world.take_calls());
        rust.push(Value::Object(map));
    }
    assert_eq!(ledger("syncs", node, &rust), 36);
}

#[tokio::test]
async fn ticket_t_59_sync_update_check_matches_node() {
    let golden = golden();
    let world = World::new(&golden);
    let node = &golden["updates"];
    let (mut checked, mut skipped, mut metadata, mut events) =
        (Vec::new(), Vec::new(), Map::new(), Vec::new());
    for entry in node["rows"].as_array().unwrap() {
        let (id, name) = (&entry["id"], &entry["name"]);
        let status = match entry["row"].is_null() {
            true => None,
            false => {
                let row: GitSourceRow = serde_json::from_value(entry["row"].clone()).unwrap();
                sync::check_source_update(&world, &row).await
            }
        };
        let Some(status) = status else {
            skipped.push(json!({ "source_id": id, "name": name, "reason": "not_git" }));
            continue;
        };
        metadata.insert(id.to_string(), json!(status));
        if sync::update_event_due(entry["update_status"].as_str(), status) {
            events.push(id.clone());
        }
        checked.push(json!({ "source_id": id, "name": name, "update_status": status }));
    }
    let updates_available = checked
        .iter()
        .filter(|entry| entry["update_status"] == json!(RemoteUpdateStatus::UpdatesAvailable))
        .count();
    let rust = json!({
        "rows": node["rows"],
        "checked_count": checked.len(),
        "checked": &checked,
        "skipped": skipped,
        "updates_available": updates_available,
        "metadata": metadata,
        "events": &events,
        "calls": world.take_calls(),
    });
    assert_eq!(&rust, node, "update check differs from Node");
    assert_eq!((checked.len(), events.len()), (8, 1));
}
