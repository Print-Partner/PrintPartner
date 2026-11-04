use super::*;
use crate::scratch::Scratch;
use ownership::cutover::{self, CutoverReceipt, DrainEvidence};

#[tokio::test]
async fn ticket_t_3_drain_legal_body_retains_admission_until_consumed_or_dropped() {
    let root = Scratch::new("pp-t3-legal-body-");
    let index = ownership::manifest::admit_bundled_manifest().unwrap();
    let operation = "http:GET /legal/summary current";
    let parity = serde_json::json!({
        "schema":"pp-operation-parity-v1", "operation":operation,
        "manifest_sha256":index.digest(), "fixtures":4, "passed":4, "failed":0,
    });
    let plan = cutover::plan(
        Some(&index),
        &OperationOwners::node_only(),
        operation,
        &parity.to_string(),
    )
    .unwrap();
    assert!(matches!(
        plan.commit(
            &root,
            DrainEvidence {
                tracked_idle: true,
                gateway_idle: true,
                compat: Some(pp_compat::CompatExit::Graceful),
            }
        ),
        CutoverReceipt::Committed { .. }
    ));
    let (owners, _) = cutover::recover(&root, Some(&index)).unwrap();

    for path in ["/legal/summary", "/api/v1/legal/summary"] {
        for consume in [true, false] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut gateway = Gateway::start(
                listener,
                root.to_path_buf(),
                CompatHandle::unpublished(),
                &owners,
            )
            .unwrap();
            let cookie = gateway.test_session().await;
            let request = || {
                Request::builder()
                    .uri(path)
                    .header("host", &gateway.state.host)
                    .header("origin", gateway.origin())
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap()
            };
            let response = handle(State(gateway.state.clone()), request()).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert!(
                !gateway.wait_idle(Duration::ZERO).await,
                "Rust legal response became idle before body completion: {path}",
            );
            gateway.begin_drain();
            assert!(!gateway.wait_idle(Duration::ZERO).await);
            assert_eq!(
                handle(State(gateway.state.clone()), request())
                    .await
                    .status(),
                StatusCode::SERVICE_UNAVAILABLE
            );
            if consume {
                let bytes = response.into_body().collect().await.unwrap().to_bytes();
                assert_eq!(
                    &bytes[..],
                    include_bytes!("../../../../web/apps/server/src/data/legal/LICENSE-SUMMARY.md")
                );
            } else {
                drop(response);
            }
            assert!(gateway.wait_idle(Duration::ZERO).await);
            gateway.stop().await.unwrap();
            println!(
                "legal body {path}: consume={consume}; held through drain; idle after release"
            );
        }
    }
}
