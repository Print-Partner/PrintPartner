//! `propose_set_build_source_roles` and its `confirm_apply` (GRE-1300 PR 1),
//! through the MCP router with an API key, on a Build whose planning brief
//! `propose_create_build` created.
use super::create_build::{build_planning, confirm, execute, json_call, orders_on, pending};
use super::*;
use pp_storage::read_model::planning::{self, BuildPlanningPhase, SourceRole};

const TOOL: &str = "propose_set_build_source_roles";
const DRAFT: i64 = 7;

struct Seeded {
    server: McpServer,
    headers: Vec<(String, String)>,
    build: u64,
    /// Library Sources S1 and S2.
    sources: [i64; 2],
}

impl Seeded {
    /// Build B from `propose_create_build`, Library Sources S1 (role `mod`)
    /// and S2 (role `overlay`), one layer of B on S1, and B's brief given
    /// evidence E1 (S1) and E2 (S2) with no roles and selected draft D.
    async fn new() -> Self {
        let (server, headers) = orders_on().await;
        let args = json!({ "name": "Voron for Sam", "request": "Voron 2.4, black ABS" });
        let (proposal, _) = json_call(&server, &headers, "propose_create_build", args).await;
        let id = proposal["action"]["id"].as_str().unwrap().to_owned();
        let (created, error) = confirm(&server, &headers, &id).await;
        assert!(!error && created["ok"] == true, "{created}");
        let build = created["result"]["plan_id"].as_u64().unwrap();
        execute(
            &server,
            "INSERT INTO projects(tenant_id,name,url,role) VALUES \
             ('default','S1','https://github.com/example/s1','mod'), \
             ('default','S2','https://github.com/example/s2','overlay');",
        );
        let sources = [seeded_source(&server, "S1"), seeded_source(&server, "S2")];
        execute(
            &server,
            &format!(
                "INSERT INTO profile_layers(tenant_id,profile_id,layer_order,layer_type,project_id) \
                 VALUES('default',{build},0,'base',{});",
                sources[0]
            ),
        );
        let seeded = Self {
            sources,
            server,
            headers,
            build,
        };
        let mut brief = seeded.brief();
        brief["evidence"] = json!([
            evidence("E1", seeded.sources[0]),
            evidence("E2", seeded.sources[1]),
        ]);
        brief["draft_id"] = json!(DRAFT);
        seeded.store(&brief);
        seeded
    }

    fn database(&self) -> rusqlite::Connection {
        let path = self.server._directory.join("print-partner.db");
        let connection = rusqlite::Connection::open(path).unwrap();
        connection.busy_timeout(Duration::from_secs(5)).unwrap();
        connection
    }

    fn key(&self) -> String {
        format!("build_planning.v1.{}", self.build)
    }

    fn brief(&self) -> Value {
        let text: String = self
            .database()
            .query_row(
                "SELECT value FROM app_settings WHERE tenant_id='default' AND key=?1",
                [self.key()],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn store(&self, brief: &Value) {
        self.database()
            .execute(
                "UPDATE app_settings SET value=?2 WHERE tenant_id='default' AND key=?1",
                rusqlite::params![self.key(), brief.to_string()],
            )
            .unwrap();
    }

    /// Layers, every setting (the brief among them) and decisions.
    fn state(&self) -> Vec<Vec<Vec<String>>> {
        pp_testkit::mcp_planning::state(&self.server._directory.join("print-partner.db")).unwrap()
    }

    /// The Library role of each Source and B's Plan revision count: the
    /// Accepted Plan is none and stays none.
    fn untouched(&self) -> (Vec<String>, i64) {
        let database = self.database();
        let roles = self
            .sources
            .iter()
            .map(|id| {
                database
                    .query_row("SELECT role FROM projects WHERE id=?1", [id], |row| {
                        row.get(0)
                    })
                    .unwrap()
            })
            .collect();
        let revisions = database
            .query_row(
                "SELECT COUNT(*) FROM plan_revisions WHERE profile_id=?1",
                [self.build as i64],
                |row| row.get(0),
            )
            .unwrap();
        (roles, revisions)
    }

    async fn propose(&self, roles: Value) -> (Value, bool) {
        let args = json!({ "plan_id": self.build, "roles": roles });
        json_call(&self.server, &self.headers, TOOL, args).await
    }

    /// Proposes `roles`, checks that propose wrote nothing, and returns the
    /// action id.
    async fn proposed(&self, roles: Value) -> String {
        let before = self.state();
        let (proposal, error) = self.propose(roles).await;
        assert!(!error, "{proposal}");
        assert_eq!(proposal["status"], "proposed", "{proposal}");
        assert_eq!(self.state(), before, "propose wrote nothing");
        proposal["action"]["id"].as_str().unwrap().to_owned()
    }
}

fn seeded_source(server: &McpServer, name: &str) -> i64 {
    let path = server._directory.join("print-partner.db");
    rusqlite::Connection::open(path)
        .unwrap()
        .query_row("SELECT id FROM projects WHERE name=?1", [name], |row| {
            row.get(0)
        })
        .unwrap()
}

fn evidence(id: &str, source: i64) -> Value {
    let url = format!("https://github.com/example/{}", id.to_lowercase());
    json!({ "id": id, "url": url, "normalized_url": url, "kind": "canonical_design", "source_id": source })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_source_roles_set_the_roles_and_clear_the_selected_draft() {
    let seeded = Seeded::new().await;
    let library = seeded.untouched();
    let layers = seeded.state()[0].clone();
    let b_layers: i64 = seeded
        .database()
        .query_row(
            "SELECT COUNT(*) FROM profile_layers WHERE profile_id=?1",
            [seeded.build as i64],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(b_layers, 1, "B has one layer");
    let planning::Outcome::BuildPlanning(response) =
        build_planning(&seeded.server, seeded.build).await
    else {
        panic!("the brief reads");
    };
    assert_eq!(response.planning.unwrap().brief.draft_id, Some(DRAFT));
    let id = seeded
        .proposed(json!([
            { "evidence_id": "E1", "role": "structural_base" },
            { "evidence_id": "E2", "role": "addon" },
        ]))
        .await;

    let (confirmed, error) = confirm(&seeded.server, &seeded.headers, &id).await;
    assert!(!error, "{confirmed}");
    assert_eq!(
        confirmed,
        json!({ "ok": true, "result": { "updated": 2, "evidence": [
            { "id": "E1", "source_id": seeded.sources[0], "source_role": "structural_base" },
            { "id": "E2", "source_id": seeded.sources[1], "source_role": "addon" },
        ] } })
    );
    assert!(pending(&seeded.server, &seeded.headers).await.is_empty());

    let planning::Outcome::BuildPlanning(response) =
        build_planning(&seeded.server, seeded.build).await
    else {
        panic!("the brief reads");
    };
    let planning = response.planning.expect("B has a brief");
    let roles: Vec<_> = planning
        .brief
        .evidence
        .iter()
        .map(|item| (item.id.as_str(), item.source_role))
        .collect();
    assert_eq!(
        roles,
        [
            ("E1", Some(SourceRole::StructuralBase)),
            ("E2", Some(SourceRole::Addon)),
        ]
    );
    assert_eq!(planning.brief.draft_id, None);
    assert!(seeded.brief().get("draft_id").is_none(), "stored brief");
    assert_eq!(planning.planning_phase, BuildPlanningPhase::Preparing);
    assert_eq!(
        seeded.untouched(),
        library,
        "Library roles and Accepted Plan"
    );
    assert_eq!(seeded.state()[0], layers, "B's layers");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_source_roles_confirm_refuses_with_a_code_and_changes_nothing() {
    let seeded = Seeded::new().await;
    let before = (seeded.state(), seeded.untouched());
    let mut kept = Vec::new();
    for (roles, code) in [
        (
            json!([{ "evidence_id": "E9", "role": "addon" }]),
            "evidence_not_found",
        ),
        (
            json!([{ "evidence_id": "E1", "role": "base" }]),
            "invalid_source_role",
        ),
        (json!(["E1"]), "invalid_role_assignment"),
        (
            json!([
                { "evidence_id": "E1", "role": "structural_base" },
                { "evidence_id": "E2", "role": "structural_base" },
            ]),
            "multiple_structural_bases",
        ),
    ] {
        let id = seeded.proposed(roles.clone()).await;
        let (refused, error) = confirm(&seeded.server, &seeded.headers, &id).await;
        assert!(error, "{refused}");
        assert_eq!(refused["ok"], false, "{roles}: {refused}");
        assert_eq!(refused["code"], code, "{roles}: {refused}");
        assert!(refused["detail"].is_string(), "{refused}");
        if code == "evidence_not_found" {
            assert_eq!(refused["evidence_id"], "E9", "{refused}");
        }
        assert_eq!((seeded.state(), seeded.untouched()), before, "{roles}");
        kept.push(id);
    }
    // A refused confirm keeps its proposal pending.
    let pending: Vec<_> = pending(&seeded.server, &seeded.headers)
        .await
        .into_iter()
        .map(|action| action["id"].as_str().unwrap().to_owned())
        .collect();
    assert!(kept.iter().all(|id| pending.contains(id)), "{pending:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_source_roles_propose_refuses_with_a_code() {
    let seeded = Seeded::new().await;
    let roles = json!([{ "evidence_id": "E1", "role": "addon" }]);
    let cases = [
        (json!({ "roles": roles }), "invalid_plan_id"),
        (json!({ "plan_id": 0, "roles": roles }), "invalid_plan_id"),
        (
            json!({ "plan_id": seeded.build.to_string(), "roles": roles }),
            "invalid_plan_id",
        ),
        (json!({ "plan_id": seeded.build }), "invalid_roles"),
        (
            json!({ "plan_id": seeded.build, "roles": { "E1": "addon" } }),
            "invalid_roles",
        ),
        (
            json!({ "plan_id": seeded.build + 1, "roles": roles }),
            "no_planning_brief",
        ),
    ];
    for (args, code) in cases {
        let (refused, error) = json_call(&seeded.server, &seeded.headers, TOOL, args.clone()).await;
        assert!(error, "{args}: {refused}");
        assert_eq!(refused["code"], code, "{args}: {refused}");
    }

    // A Build with no brief is refused before anything is proposed.
    execute(
        &seeded.server,
        &format!("DELETE FROM app_settings WHERE key='{}';", seeded.key()),
    );
    let (refused, error) = seeded.propose(roles).await;
    assert!(error);
    assert_eq!(refused["code"], "no_planning_brief", "{refused}");
    assert!(pending(&seeded.server, &seeded.headers).await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_source_roles_without_a_key_is_refused_and_proposes_nothing() {
    let seeded = Seeded::new().await;
    let before = seeded.state();
    let arguments =
        json!({ "plan_id": seeded.build, "roles": [{ "evidence_id": "E1", "role": "addon" }] });
    let status = seeded
        .server
        .unkeyed_call_status(&seeded.headers, TOOL, arguments)
        .await;
    assert_eq!(status, 401);
    assert!(pending(&seeded.server, &seeded.headers).await.is_empty());
    assert_eq!(seeded.state(), before);
}
