//! `GET /plans/{id}/build-planning` and `GET /plans/{id}/decisions` through
//! the Rust-only read composition, replayed against Node's planning-http
//! golden (GRE-1119): the same seeded rows and Source files, the same
//! responses, a session on every request, and nothing written.

#[cfg(not(feature = "desktop_rebuild"))]
#[test]
fn build_planning_read_needs_the_rebuild_feature() {}

#[cfg(feature = "desktop_rebuild")]
mod with_feature {
    use pp_api::{
        fixtures::{Served, StateGraph},
        problem::without_envelope,
    };
    use pp_core::DesktopRebuild;
    use pp_core::rebuild_api::{
        AuthHttpConfig, AuthRoutes, CookieTransport, ProviderClient, ResetMailer, read_router,
    };
    use pp_storage::{
        Limits, WriterOwner,
        auth::{
            AuthPolicy, FirstUserTenant, RegistrationPolicy, SessionTenantPolicy,
            rebuild::AuthLifetimes,
        },
    };
    use rusqlite::types::Value as Sql;
    use serde_json::{Value, json};
    use std::{
        path::Path,
        sync::{Arc, Mutex},
        time::Duration,
    };

    const GOLDEN: &str = include_str!("../../../../fixtures/parity/planning-http/node.json");

    fn auth_routes(origin: &str) -> AuthRoutes {
        AuthRoutes {
            http: AuthHttpConfig::new(origin, CookieTransport::LoopbackHttp, false, None, false)
                .unwrap(),
            policy: AuthPolicy {
                registration: RegistrationPolicy::FirstAccountOnly,
                first_user: FirstUserTenant::NewUser,
                session_tenant: SessionTenantPolicy::SingleAccountDefault,
            },
            lifetimes: AuthLifetimes::shorter(Duration::from_secs(3600), None).unwrap(),
            providers: ProviderClient::new(None, None).unwrap(),
            mail: ResetMailer::disabled(),
        }
    }

    /// The golden's Source files and SQL rows, with `{data}` as `data`.
    fn seed(golden: &Value, data: &Path) {
        for file in golden["files"].as_array().unwrap() {
            let path = data.join(file[0].as_str().unwrap());
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            match &file[1] {
                Value::String(content) => std::fs::write(&path, content).unwrap(),
                link => std::os::unix::fs::symlink(link["link"].as_str().unwrap(), &path).unwrap(),
            }
        }
        let db = rusqlite::Connection::open(data.join("print-partner.db")).unwrap();
        db.busy_timeout(Duration::from_secs(5)).unwrap();
        let data = data.to_str().unwrap();
        for row in golden["sql"].as_array().unwrap() {
            let values: Vec<Sql> = row[1]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| match value {
                    Value::Null => Sql::Null,
                    Value::Number(number) => Sql::Integer(number.as_i64().unwrap()),
                    Value::String(text) => Sql::Text(text.replace("{data}", data)),
                    other => panic!("seed value {other}"),
                })
                .collect();
            db.execute(row[0].as_str().unwrap(), rusqlite::params_from_iter(values))
                .unwrap_or_else(|error| panic!("{}: {error}", row[0]));
        }
    }

    /// Node omits an absent optional brief field; Rust serves `null`.
    fn without_nulls(value: &mut Value) {
        match value {
            Value::Object(object) => {
                object.retain(|_, field| !field.is_null());
                object.values_mut().for_each(without_nulls);
            }
            Value::Array(items) => items.iter_mut().for_each(without_nulls),
            _ => {}
        }
    }

    /// Registers the session and creates the ten Plans the golden names, in
    /// Node's id order. Returns the session cookie.
    async fn create_plans(client: &reqwest::Client, origin: &str) -> String {
        let registered = client
            .post(format!("{origin}/auth/register"))
            .header("Origin", origin)
            .json(&json!({"email": "planning@example.test", "password": "password-1234"}))
            .send()
            .await
            .unwrap();
        let cookie = registered.headers()["set-cookie"].to_str().unwrap();
        let cookie = cookie.split(';').next().unwrap().to_owned();
        let names = [
            "Synced kept",
            "Synced changed",
            "Applied",
            "Abandoned",
            "Missing draft",
            "Claims",
            "No brief",
            "Invalid brief",
            "Foreign brief",
            "Corrupt draft",
        ];
        for (index, name) in names.into_iter().enumerate() {
            let created: Value = client
                .post(format!("{origin}/api/v2/plans"))
                .header("Origin", origin)
                .header("Cookie", &cookie)
                .json(&json!({"name": name}))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            assert_eq!(created["id"], index + 1, "{name} gets Node's id");
        }
        cookie
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn build_planning_read_matches_node_and_writes_nothing() {
        let golden: Value = serde_json::from_str(GOLDEN).unwrap();
        let data = tempfile::tempdir().unwrap();
        let owner = Arc::new(Mutex::new(
            WriterOwner::open(data.path(), Limits::default()).unwrap().0,
        ));
        let served = Served::start(|origin| {
            read_router(
                DesktopRebuild::from_value(Some("1")),
                Arc::clone(&owner),
                auth_routes(origin),
                origin,
                "default",
                None,
            )
            .unwrap()
            .expect("flag on mounts the planning reads")
        })
        .await
        .unwrap();
        let (client, origin) = (served.client.clone(), served.origin.clone());
        let send = |path: String, cookie: Option<String>| {
            let mut request = client
                .get(format!("{origin}{path}"))
                .header("Origin", &origin);
            if let Some(cookie) = cookie {
                request = request.header("Cookie", cookie);
            }
            async move {
                let response = request.send().await.unwrap();
                let status = response.status().as_u16();
                (
                    status,
                    without_envelope(response.json().await.unwrap_or(Value::Null)),
                )
            }
        };
        let cookie = create_plans(&client, &origin).await;
        seed(&golden, data.path());
        let before = StateGraph::capture(data.path()).unwrap();

        let cases = golden["cases"].as_array().unwrap();
        for case in cases {
            let (id, path) = (case["id"].as_str().unwrap(), case["path"].as_str().unwrap());
            let node = &case["node"];
            assert_eq!(
                send(path.into(), None).await.0,
                401,
                "{id}: a session is required"
            );
            let (status, mut body) = send(path.into(), Some(cookie.clone())).await;
            assert_eq!(status, node["status"], "{id}: {body}");
            match status {
                200 => {
                    if let Some(brief) = body.pointer_mut("/planning/brief") {
                        without_nulls(brief);
                    }
                    assert_eq!(body, node["body"], "{id}");
                }
                // Node's error is Fastify's default 500 body; Rust's is a problem.
                500 => assert_eq!(body["detail"], "Internal Server Error", "{id}"),
                _ => assert_eq!(body["detail"], node["body"]["detail"], "{id}"),
            }
            assert_eq!(case["changed"], json!([]), "{id}: Node wrote nothing");
        }
        assert!(cases.len() >= 27);
        assert_eq!(
            before.changed(&StateGraph::capture(data.path()).unwrap()),
            Vec::<String>::new(),
            "no table or file changed"
        );
        served.stop().await.unwrap();
        Arc::into_inner(owner)
            .expect("routers dropped")
            .into_inner()
            .unwrap()
            .shutdown()
            .unwrap();
    }
}
