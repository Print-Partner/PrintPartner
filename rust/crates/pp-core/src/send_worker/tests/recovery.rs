use super::*;
use pp_storage::send_queue::{INTERRUPTED_ERROR, UNKNOWN_OUTCOME_ERROR};
use pp_testkit::moonraker::fault::{FaultPrinter, Reply as FaultReply};

const WAIT: Duration = Duration::from_secs(5);

struct Recovery {
    generation: i64,
    text: &'static str,
    error: &'static str,
}

const WORKER_RECORDED_UNKNOWN: Recovery = Recovery {
    generation: 1,
    text: "Effect outcome unknown",
    error: UNKNOWN_OUTCOME_ERROR,
};

const RESTART_RECOVERED_INTERRUPTION: Recovery = Recovery {
    generation: 2,
    text: "The send may have reached the printer; it is never resent automatically. Inspect and reconcile",
    error: INTERRUPTED_ERROR,
};

impl Farm {
    fn reopen(self) -> Self {
        let Self { root, owner, token } = self;
        owner.shutdown().unwrap();
        let (owner, _) =
            WriterOwner::open_with_job_feature_admission(root.path(), Limits::default()).unwrap();
        Self { root, owner, token }
    }

    fn document(&self) -> Value {
        let text: String = self.database().query_row(
            "SELECT j.document FROM durable_jobs j JOIN printer_send_queue q ON j.id=q.upload_job_id WHERE q.id='q1'",
            [], |row| row.get(0),
        ).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn assert_intent(&self, operation: &str) -> Value {
        let document = self.document();
        assert_eq!(document["state"], "effect_admitted");
        assert_eq!(document["effects"].as_array().unwrap().len(), 1);
        let effect = &document["effects"][0];
        assert_eq!(effect["intent"]["operation"], operation);
        assert_eq!(effect["intent"]["target"], "p-moon");
        assert_eq!(effect["confirmed"], false);
        assert!(effect["receipt"].is_null());
        document
    }

    fn staged(&self) -> Vec<u8> {
        std::fs::read(self.exports().join("printer-uploads/q1/a.gcode")).unwrap()
    }

    async fn assert_stable_reopen(
        self,
        printer: &FaultPrinter,
        admitted: &Value,
        effects: usize,
        expected: &Recovery,
    ) -> Value {
        let farm = self.reopen();
        let reopened = farm.document();
        assert_eq!(reopened["effects"], admitted["effects"]);
        assert_eq!(reopened["generation"], expected.generation);
        assert_eq!(reopened["recovery"], expected.text);
        farm.assert_not_resent(printer, effects, expected.error)
            .await;
        let farm = farm.reopen();
        assert_eq!(
            farm.document(),
            reopened,
            "a repeated reopen must not touch the job"
        );
        farm.assert_not_resent(printer, effects, expected.error)
            .await;
        reopened
    }

    async fn assert_not_resent(&self, printer: &FaultPrinter, effects: usize, error: &str) {
        let before = self.document();
        let worker = self.worker();
        for _ in 0..3 {
            assert_eq!(
                tokio::time::timeout(WAIT, worker.run_next())
                    .await
                    .unwrap()
                    .unwrap(),
                None
            );
        }
        assert!(matches!(
            self.send("q1"),
            Reply::Refused {
                code: Some("send_outcome_unknown"),
                ..
            }
        ));
        assert_eq!(
            self.document(),
            before,
            "refused redispatch must not rewrite the job"
        );
        assert_eq!(printer.accepted(), effects);
        assert_eq!(printer.received(), effects, "no repeated HTTP request");
        let (state, recorded, ..) = self.outcome("q1");
        assert_eq!(state, "error");
        assert_eq!(recorded.as_deref(), Some(error));
        assert_eq!(before["state"], "reconciliation_required");
        assert!(before["public_result"].is_null());
        assert!(
            self.links().is_null(),
            "uncertainty must not create a Checkoff link"
        );
        assert_eq!(
            before["effects"][0]["intent"]["content_hash"],
            hex::encode(Sha256::digest(self.staged()))
        );
    }
}

struct RunningSend {
    task: Option<tokio::task::JoinHandle<anyhow::Result<Option<Sent>>>>,
}

impl RunningSend {
    fn spawn(worker: SendWorker) -> Self {
        Self {
            task: Some(tokio::spawn(async move { worker.run_next().await })),
        }
    }

    async fn settled(mut self) -> Sent {
        let task = self.task.as_mut().unwrap();
        match tokio::time::timeout(WAIT, task).await {
            Ok(outcome) => {
                self.task = None;
                outcome.unwrap().unwrap().unwrap()
            }
            Err(_) => {
                let _ = self.abort_and_join().await;
                panic!("send worker did not settle within {WAIT:?}");
            }
        }
    }

    async fn cancel(mut self) {
        let outcome = self.abort_and_join().await.unwrap();
        assert!(outcome.unwrap_err().is_cancelled());
    }

    async fn abort_and_join(
        &mut self,
    ) -> Result<
        Result<anyhow::Result<Option<Sent>>, tokio::task::JoinError>,
        tokio::time::error::Elapsed,
    > {
        let task = self.task.as_mut().unwrap();
        task.abort();
        let outcome = tokio::time::timeout(WAIT, task).await;
        if outcome.is_ok() {
            self.task = None;
        }
        outcome
    }
}

impl Drop for RunningSend {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

fn begin_send(printer: &FaultPrinter, start: bool) -> (Farm, RunningSend, Value) {
    let farm = Farm::open(&printer.url(), &[("q1", start, true)]);
    assert!(matches!(farm.send("q1"), Reply::Dispatched { .. }));
    let task = RunningSend::spawn(farm.worker());
    assert_upload(&printer.request().unwrap(), &farm.staged());
    let operation = if start {
        "printer_upload_and_start"
    } else {
        "printer_upload"
    };
    let admitted = farm.assert_intent(operation);
    assert_eq!(admitted["generation"], 1);
    assert_eq!(
        printer.accepted(),
        0,
        "no fake effect before committed intent"
    );
    (farm, task, admitted)
}

fn assert_upload(request: &pp_testkit::moonraker::fault::Request, staged: &[u8]) {
    assert!(
        request.line.starts_with("POST /server/files/upload "),
        "{}",
        request.line
    );
    assert!(staged.starts_with(b"G28\nG1 X10"));
    assert_eq!(
        request
            .body
            .windows(staged.len())
            .filter(|bytes| *bytes == staged)
            .count(),
        1,
        "the whole staged file, sent once"
    );
    assert!(
        request.body.ends_with(b"--\r\n"),
        "complete multipart terminator required"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ticket_t_28_recovery_lost_upload_reply_survives_repeated_restart() {
    let printer = FaultPrinter::start().unwrap();
    let (farm, task, admitted) = begin_send(&printer, false);
    printer.accept(FaultReply::Lost).unwrap();
    assert!(matches!(task.settled().await, Sent::Unknown(_)));
    let settled = farm.document();
    assert_eq!(settled["effects"], admitted["effects"]);
    let reopened = farm
        .assert_stable_reopen(&printer, &admitted, 1, &WORKER_RECORDED_UNKNOWN)
        .await;
    assert_eq!(reopened, settled, "restart leaves a recorded unknown alone");
}

#[tokio::test(flavor = "multi_thread")]
async fn ticket_t_28_recovery_lost_start_reply_never_reuploads_or_restarts() {
    let printer = FaultPrinter::start().unwrap();
    let (farm, task, _) = begin_send(&printer, true);
    printer.accept(FaultReply::Accepted).unwrap();
    let start = printer.request().unwrap();
    assert!(
        start
            .line
            .starts_with("POST /printer/print/start?filename=a.gcode ")
    );
    assert!(start.body.is_empty());
    let admitted = farm.assert_intent("printer_upload_and_start");
    assert_eq!(printer.accepted(), 1);
    printer.accept(FaultReply::Lost).unwrap();
    assert!(matches!(task.settled().await, Sent::Unknown(_)));
    let settled = farm.document();
    let reopened = farm
        .assert_stable_reopen(&printer, &admitted, 2, &WORKER_RECORDED_UNKNOWN)
        .await;
    assert_eq!(reopened, settled, "restart leaves a recorded unknown alone");
}

#[tokio::test(flavor = "multi_thread")]
async fn ticket_t_28_recovery_interruption_after_remote_acceptance_keeps_intent() {
    let printer = FaultPrinter::start().unwrap();
    let (farm, task, admitted) = begin_send(&printer, false);
    printer.accept(FaultReply::Hold).unwrap();
    assert_eq!(printer.accepted(), 1);
    task.cancel().await;
    assert_eq!(
        farm.document(),
        admitted,
        "no completion checkpoint was written"
    );
    printer.accept(FaultReply::Lost).unwrap();
    farm.assert_stable_reopen(&printer, &admitted, 1, &RESTART_RECOVERED_INTERRUPTION)
        .await;
}
