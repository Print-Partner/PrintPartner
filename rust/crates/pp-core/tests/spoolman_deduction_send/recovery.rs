use super::{
    Api, DeductionWorker, Fake, Fixture, Gate, Mode, SESSION, SpoolmanInventory, WriterOwner,
    assert_one_put, confirm, desktop_policy, get_deduction, http_client, post_answer, prepared,
    requests,
};
use pp_core::{DesktopRebuild, rebuild_api::product_router};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::oneshot, time::timeout};

const BATCH: &str = "/api/v2/printer-checkoff/verify-batch";
const UNDO: &str = "/api/v2/printer-checkoff/verify-batch/undo";
const WAIT: Duration = Duration::from_secs(5);

struct Runtime {
    api: Api,
    worker: DeductionWorker,
    owner: Arc<Mutex<WriterOwner>>,
}

impl Runtime {
    async fn start(owner: WriterOwner) -> Self {
        let worker =
            DeductionWorker::new(&owner, desktop_policy(), SpoolmanInventory::new().unwrap())
                .unwrap();
        let owner = Arc::new(Mutex::new(owner));
        let api = Api::serve(|origin| {
            product_router(
                DesktopRebuild::from_value(Some("1")),
                Arc::clone(&owner),
                origin,
                None,
            )
            .unwrap()
            .unwrap()
        })
        .await;
        Self { api, worker, owner }
    }

    async fn shutdown(self) {
        let Self {
            mut api,
            worker,
            owner,
        } = self;
        api.stop.take().unwrap().send(()).unwrap();
        timeout(WAIT, &mut api.task).await.unwrap().unwrap();
        drop(worker);
        Arc::try_unwrap(owner)
            .ok()
            .unwrap()
            .into_inner()
            .unwrap()
            .shutdown()
            .unwrap();
    }

    async fn post(&self, path: &str, body: Value) -> Value {
        let response = timeout(
            WAIT,
            http_client()
                .post(format!("{}{path}", self.api.origin))
                .header("Cookie", format!("pp_session={SESSION}"))
                .header("Origin", &self.api.origin)
                .json(&body)
                .send(),
        )
        .await
        .expect("Checkoff route timed out")
        .unwrap();
        let status = response.status().as_u16();
        let body: Value = timeout(WAIT, response.json()).await.unwrap().unwrap();
        assert_eq!(status, 200, "{body}");
        body
    }

    async fn verify(&self, key: &str) -> Value {
        self.post(
            BATCH,
            json!({
                "idempotency_key": key,
                "beds": [{ "link_id": "link-verify", "decisions": [confirm(1, 1), confirm(1, 2)] }]
            }),
        )
        .await
    }

    async fn detail(&self, id: i64) -> Value {
        let (status, body) = timeout(WAIT, get_deduction(&self.api.origin, id))
            .await
            .unwrap();
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["deduction"]["id"], id);
        body
    }

    async fn assert_idle(&self, fake: &Fake) {
        let before = requests(fake);
        assert!(
            !timeout(WAIT, self.worker.run_next())
                .await
                .unwrap()
                .unwrap()
        );
        assert_eq!(requests(fake), before);
    }
}

fn intent(fixture: &Fixture, expected_state: &str, expected_before: Option<f64>) -> i64 {
    let db = fixture.sql();
    let mut statement = db
        .prepare(
            "SELECT id, state, integration_id, spool_id, grams, units, spool_grams_before
             FROM spoolman_deductions WHERE tenant_id = 'default'",
        )
        .unwrap();
    let mut rows = statement.query([]).unwrap();
    let row = rows.next().unwrap().expect("committed deduction intent");
    assert_eq!(row.get::<_, String>(1).unwrap(), expected_state);
    assert_eq!(row.get::<_, String>(2).unwrap(), "sm-1");
    assert_eq!(row.get::<_, i64>(3).unwrap(), 7);
    assert_eq!(row.get::<_, f64>(4).unwrap(), 20.0);
    assert_eq!(row.get::<_, i64>(5).unwrap(), 2);
    assert_eq!(row.get::<_, Option<f64>>(6).unwrap(), expected_before);
    let id = row.get(0).unwrap();
    assert!(
        rows.next().unwrap().is_none(),
        "more than one deduction intent"
    );
    id
}

async fn arrived(signal: oneshot::Receiver<()>) {
    timeout(WAIT, signal)
        .await
        .expect("fake never received the deduction")
        .unwrap();
}

fn assert_deducted_once(fake: &Fake) {
    assert_one_put(fake, 20.0);
    assert!((fake.remaining(7) - 492.4).abs() < 1e-6);
}

async fn assert_unknown(runtime: &Runtime, id: i64, failure: &str) {
    let detail = runtime.detail(id).await;
    assert_eq!(detail["deduction"]["state"], "unknown");
    assert_eq!(detail["deduction"]["failure"], failure);
    assert_eq!(
        detail["grams_before"],
        json!({"kind": "known", "grams": 512})
    );
    assert_eq!(detail["grams_now"], json!({"kind": "known", "grams": 492}));
}

#[tokio::test]
async fn lost_spoolman_acknowledgement_keeps_committed_intent_and_never_redispatches() {
    let fake = Fake::start(Mode::Drop).await;
    let (fixture, owner) = prepared(&fake.origin);
    let runtime = Runtime::start(owner).await;
    runtime.verify("recovery-lost-ack").await;
    let id = intent(&fixture, "pending", None);
    assert!(fake.puts().is_empty());

    let (before_effect, allow_effect) = Gate::hold(&fake.gates.before_put);
    let (sent, ()) = tokio::join!(runtime.worker.run_next(), async {
        arrived(before_effect).await;
        assert_eq!(intent(&fixture, "sending", Some(512.4)), id);
        assert_eq!(fake.remaining(7), 512.4);
        assert_one_put(&fake, 20.0);
        allow_effect.send(()).unwrap();
    });
    assert!(sent.unwrap());
    assert_unknown(&runtime, id, "reply_lost").await;
    runtime.assert_idle(&fake).await;
    runtime.shutdown().await;

    let reopened = Runtime::start(fixture.open()).await;
    assert_unknown(&reopened, id, "reply_lost").await;
    reopened.assert_idle(&fake).await;
    assert_deducted_once(&fake);
    reopened.shutdown().await;
}

#[tokio::test]
async fn interrupting_the_worker_after_the_effect_recovers_the_sending_row_as_unknown() {
    let fake = Fake::start(Mode::Apply).await;
    let (fixture, owner) = prepared(&fake.origin);
    let runtime = Runtime::start(owner).await;
    runtime.verify("recovery-interrupted").await;
    let id = intent(&fixture, "pending", None);

    let (effect_applied, allow_reply) = Gate::hold(&fake.gates.after_put);
    tokio::select! {
        result = runtime.worker.run_next() => panic!("worker finished before acknowledgement: {result:?}"),
        () = arrived(effect_applied) => {}
    }
    assert_eq!(intent(&fixture, "sending", Some(512.4)), id);
    assert_deducted_once(&fake);
    drop(allow_reply);
    runtime.shutdown().await;

    let reopened = Runtime::start(fixture.open()).await;
    assert_unknown(&reopened, id, "interrupted").await;
    reopened.assert_idle(&fake).await;
    assert_deducted_once(&fake);
    reopened.shutdown().await;
}

#[tokio::test]
async fn undo_and_reverify_cannot_reverse_or_repeat_an_acknowledged_spoolman_effect() {
    let fake = Fake::start(Mode::Apply).await;
    let (fixture, owner) = prepared(&fake.origin);
    let runtime = Runtime::start(owner).await;
    let verified = runtime.verify("recovery-acknowledged").await;
    let id = intent(&fixture, "pending", None);
    assert!(
        timeout(WAIT, runtime.worker.run_next())
            .await
            .unwrap()
            .unwrap()
    );
    assert_eq!(intent(&fixture, "applied", Some(512.4)), id);
    assert_deducted_once(&fake);

    let before = requests(&fake);
    let token = verified["undo_token"]
        .as_str()
        .expect("Checkoff undo token");
    let undone = runtime.post(UNDO, json!({"undo_token": token})).await;
    assert_eq!(undone["undo_token"], token);
    assert!(undone["undone_at"].is_string());
    assert_eq!(intent(&fixture, "applied", Some(512.4)), id);
    let verified_again = runtime.verify("recovery-reverified").await;
    assert_eq!(verified_again["units_confirmed"], 2);
    assert_eq!(intent(&fixture, "applied", Some(512.4)), id);
    runtime.assert_idle(&fake).await;
    assert_eq!(requests(&fake), before);
    assert_deducted_once(&fake);
    runtime.shutdown().await;

    let reopened = Runtime::start(fixture.open()).await;
    let (status, body) = timeout(
        WAIT,
        post_answer(&reopened.api.origin, id, json!({"answer": "retry"})),
    )
    .await
    .unwrap();
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["code"], "deduction_state");
    assert_eq!(reopened.detail(id).await["deduction"]["state"], "applied");
    reopened.assert_idle(&fake).await;
    assert_deducted_once(&fake);
    reopened.shutdown().await;
}
