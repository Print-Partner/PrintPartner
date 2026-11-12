//! The production worker, driver and checked HTTP transport, with a fake peer.
use super::*;
use pp_testkit::moonraker::fault::{FaultPrinter, Reply as FaultReply};

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

    async fn assert_not_resent(&self, printer: &FaultPrinter, effects: usize) {
        let before = self.document();
        let worker = self.worker();
        for _ in 0..3 {
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(5), worker.run_next())
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
        assert_eq!(self.outcome("q1").0, "error");
        assert_eq!(before["state"], "reconciliation_required");
        assert!(before["public_result"].is_null());
        assert!(
            self.links().is_null(),
            "uncertainty must not create a Checkoff link"
        );
        let retained = std::fs::read(self.exports().join("printer-uploads/q1/a.gcode")).unwrap();
        assert_eq!(
            before["effects"][0]["intent"]["content_hash"],
            hex::encode(Sha256::digest(retained))
        );
    }
}

type RunningSend = tokio::task::JoinHandle<anyhow::Result<Option<Sent>>>;

async fn settled(task: RunningSend) -> Sent {
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .unwrap()
}

fn begin_send(printer: &FaultPrinter, start: bool) -> (Farm, RunningSend, Value) {
    let farm = Farm::open(&printer.url(), &[("q1", start, true)]);
    assert!(matches!(farm.send("q1"), Reply::Dispatched { .. }));
    let worker = farm.worker();
    let task = tokio::spawn(async move { worker.run_next().await });
    assert_upload(&printer.request().unwrap());
    let operation = if start {
        "printer_upload_and_start"
    } else {
        "printer_upload"
    };
    let admitted = farm.assert_intent(operation);
    (farm, task, admitted)
}

fn assert_upload(request: &pp_testkit::moonraker::fault::Request) {
    assert!(
        request.line.starts_with("POST /server/files/upload "),
        "{}",
        request.line
    );
    assert!(
        request
            .body
            .windows(b"G28\nG1 X10".len())
            .any(|bytes| bytes == b"G28\nG1 X10")
    );
    assert!(
        request.body.ends_with(b"--\r\n"),
        "complete multipart terminator required"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ticket_t_28_recovery_lost_upload_reply_survives_repeated_restart() {
    let printer = FaultPrinter::start().unwrap();
    let (mut farm, task, admitted) = begin_send(&printer, false);
    assert_eq!(
        printer.accepted(),
        0,
        "no fake effect before committed intent"
    );
    printer.accept(FaultReply::Lost).unwrap();
    assert!(matches!(settled(task).await, Sent::Unknown(_)));
    let effect = farm.document()["effects"][0].clone();
    assert_eq!(effect, admitted["effects"][0]);
    for _ in 0..2 {
        farm = farm.reopen();
        assert_eq!(farm.document()["effects"][0], effect);
        farm.assert_not_resent(&printer, 1).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ticket_t_28_recovery_lost_start_reply_never_reuploads_or_restarts() {
    let printer = FaultPrinter::start().unwrap();
    let (mut farm, task, _) = begin_send(&printer, true);
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
    assert!(matches!(settled(task).await, Sent::Unknown(_)));
    farm = farm.reopen();
    assert_eq!(farm.document()["effects"], admitted["effects"]);
    farm.assert_not_resent(&printer, 2).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ticket_t_28_recovery_interruption_after_remote_acceptance_keeps_intent() {
    let printer = FaultPrinter::start().unwrap();
    let (farm, task, admitted) = begin_send(&printer, false);
    printer.accept(FaultReply::Hold).unwrap();
    assert_eq!(printer.accepted(), 1);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(
        farm.document(),
        admitted,
        "no completion checkpoint was written"
    );
    printer.accept(FaultReply::Lost).unwrap();
    let farm = farm.reopen();
    let recovered = farm.document();
    assert_eq!(recovered["effects"], admitted["effects"]);
    assert!(recovered["generation"].as_i64().unwrap() > admitted["generation"].as_i64().unwrap());
    assert!(
        recovered["recovery"]
            .as_str()
            .unwrap()
            .contains("never resent automatically")
    );
    farm.assert_not_resent(&printer, 1).await;
    let farm = farm.reopen();
    farm.assert_not_resent(&printer, 1).await;
}
