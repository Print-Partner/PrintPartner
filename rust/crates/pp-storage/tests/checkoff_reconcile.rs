#![cfg(feature = "desktop_rebuild")]
//! GRE-1897: the host status the watchers report moves a send's `watching`
//! Printer Checkoff link on, as Node's `reconcilePrinterCheckoff` did. Runs on
//! a copy of the required-units fixture, whose `link-verify` (host
//! `sim-moonraker`, file `plate-a.gcode`, units (1,1), (1,2), (3,0) and (3,1))
//! is put back to `watching` first.
use checkoff_apply::apply;
use checkoff_fixture::Fixture;
use pp_storage::{
    WriterFailure, WriterOwner,
    auth::{self, Secret},
    checkoff_progress::{
        Request,
        reconcile::{Change, HostReport},
    },
    read_model::{
        Credential,
        farm::{CheckoffResultState, HostState, iso_time},
    },
    spoolman_deductions::{Deduction, Filter, State},
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, SystemTime},
};
use {CheckoffResultState as Shown, HostState::*};

#[path = "common/checkoff_apply.rs"]
mod checkoff_apply;
#[path = "common/checkoff_fixture.rs"]
mod checkoff_fixture;
#[path = "common/js_json.rs"]
mod js_json;

const HOST: &str = "sim-moonraker";
const SENT: &str = "link-verify";
const FILE: &str = "plate-a.gcode";
const WAIT: Duration = Duration::from_secs(5);
const LINKS: &str =
    "SELECT value FROM app_settings WHERE tenant_id=?1 AND key='printer.checkoff_links'";
const SAVE: &str =
    "UPDATE app_settings SET value=?1 WHERE tenant_id='default' AND key='printer.checkoff_links'";
const SPOOLMAN: &str = r#"[{"id":"sm-1","type":"spoolman","name":"Spoolman","config":{"base_url":"http://127.0.0.1:7912","enabled":true},"created_at":"2026-11-01T00:00:00.000Z","updated_at":"2026-11-01T00:00:00.000Z"}]"#;

fn policy(session_tenant: auth::SessionTenantPolicy) -> auth::AuthPolicy {
    let (registration, first_user) = (
        auth::RegistrationPolicy::Open,
        auth::FirstUserTenant::NewUser,
    );
    auth::AuthPolicy {
        registration,
        first_user,
        session_tenant,
    }
}

/// The policy a signed-in session reads with.
fn sessions() -> auth::AuthPolicy {
    policy(auth::SessionTenantPolicy::AccountTenant)
}

fn session() -> Credential {
    Credential::Session(Secret::new("required-unit-fixture-secret".into()))
}

fn links(fixture: &Fixture, tenant: &str) -> Vec<Value> {
    let raw: String = fixture
        .sql()
        .query_row(LINKS, [tenant], |row| row.get(0))
        .unwrap();
    js_json::parse(&raw).as_array().unwrap().clone()
}

fn link(fixture: &Fixture, id: &str) -> Value {
    links(fixture, "default")
        .into_iter()
        .find(|link| link["id"] == id)
        .unwrap()
}

/// `(state, host_outcome)` of link `id`.
fn outcome(fixture: &Fixture, id: &str) -> (Value, Value) {
    let link = link(fixture, id);
    (link["state"].clone(), link["host_outcome"].clone())
}

/// Puts `link-verify` back to a recent `watching` send with `fields`, moves
/// the fixture's other `watching` link to host `moon-3`, adds a `watching`
/// send of the same file on host `moon-2`, and adds a copy of `link-verify`
/// patched with each of `extra`.
fn watching_with(fields: Value, extra: &[Value]) -> (Fixture, WriterOwner) {
    let fixture = Fixture::new();
    let mut all = links(&fixture, "default");
    let fresh = json!({
        "state": "watching", "created_at": iso_time(SystemTime::now()), "host_outcome": null,
        "completed_at": null, "applied_at": null, "last_progress": null,
    });
    let merge = |link: &mut Value, patch: &Value| {
        for (key, value) in patch.as_object().unwrap() {
            link[key] = value.clone();
        }
    };
    for link in &mut all {
        if link["id"] == "link-watching" {
            link["integration_id"] = "moon-3".into();
        }
        if link["id"] == "link-verify" {
            merge(link, &fresh);
            merge(link, &fields);
        }
    }
    let sent = all.iter().find(|link| link["id"] == SENT).unwrap().clone();
    let other = json!({ "id": "link-other-host", "integration_id": "moon-2" });
    for patch in std::iter::once(&other).chain(extra) {
        let mut copy = sent.clone();
        merge(&mut copy, patch);
        all.push(copy);
    }
    fixture
        .sql()
        .execute(SAVE, [serde_json::to_string(&all).unwrap()])
        .unwrap();
    let owner = fixture.open();
    (fixture, owner)
}

fn watching(fields: Value) -> (Fixture, WriterOwner) {
    watching_with(fields, &[])
}

fn report(state: HostState, filename: &str) -> HostReport {
    let filename = Some(filename.to_owned()).filter(|f| !f.is_empty());
    HostReport {
        state,
        filename,
        progress: None,
        cancelled: false,
    }
}

fn reconcile(owner: &WriterOwner, report: HostReport) -> Vec<(String, Change)> {
    let desktop = auth::AuthPolicy {
        registration: auth::RegistrationPolicy::FirstAccountOnly,
        ..policy(auth::SessionTenantPolicy::SingleAccountDefault)
    };
    owner
        .checkoff_reconcile(desktop)
        .unwrap()
        .reconcile(HOST, report, WAIT)
        .unwrap()
}

fn changed(id: &str, change: Change) -> Vec<(String, Change)> {
    vec![(id.to_owned(), change)]
}

/// The state the farm read, and so Today, shows for link `id`.
fn shown(owner: &WriterOwner, id: &str) -> Shown {
    let farm = owner.farm_reads(sessions()).unwrap();
    let farm = farm
        .read(session(), Arc::new(AtomicBool::new(false)), WAIT)
        .unwrap();
    farm.checkoff()
        .results
        .into_iter()
        .find(|result| result.id == id)
        .unwrap()
        .state
}

/// Turns the Spoolman Module on with Part 1 pinned to spool 7 and 40 g over
/// the link's 4 units, confirms units (1,1) and (1,2) of `link-verify`,
/// reconciles `finished` again, replays the burst, and returns the
/// deduction rows.
fn confirm_and_deduct(
    fixture: &Fixture,
    owner: &WriterOwner,
    finished: HostReport,
) -> Vec<Deduction> {
    fixture
        .sql()
        .execute_batch(&format!(
            "REPLACE INTO app_settings (tenant_id, key, value) VALUES ('default','feature_toggles','{{\"spoolman\":true}}');
             REPLACE INTO app_settings (tenant_id, key, value) VALUES ('default','integrations','{SPOOLMAN}');
             UPDATE parts SET spoolman_spool_id = 'spoolman:sm-1:spool:7' WHERE id = 1;
             INSERT INTO print_link_grams (tenant_id, link_id, grams, units, basis, updated_at)
             VALUES ('default', 'link-verify', 40, 4, 'estimated', '2026-11-12T00:00:00.000Z');"
        ))
        .unwrap();
    let confirm = |unit| json!({ "part_id": 1, "unit_index": unit, "result": "confirmed" });
    let beds = json!([{ "link_id": SENT, "decisions": [confirm(1), confirm(2)] }]);
    let body = json!({ "idempotency_key": "gre-1897", "beds": beds });
    let verified = apply(owner, Request::VerifyBatch { body: body.clone() });
    assert_eq!(verified["status"], 200, "{verified}");
    assert_eq!(
        reconcile(owner, finished),
        [],
        "the finish is still showing"
    );
    let replay = apply(owner, Request::VerifyBatch { body });
    assert_eq!(replay["status"], 200, "{replay}");
    let deductions = owner.spoolman_deductions(sessions()).unwrap();
    deductions
        .list(session(), Filter::default(), &AtomicBool::new(false), WAIT)
        .unwrap()
}

#[test]
fn gre_1897_a_finished_print_awaits_verify_once_and_verify_batch_deducts_once() {
    let (fixture, owner) = watching(json!({}));
    assert_eq!(shown(&owner, SENT), Shown::Watching);
    let foreign = links(&fixture, "foreign");
    let running = |progress| HostReport {
        progress: Some(progress),
        ..report(Printing, FILE)
    };
    assert_eq!(
        reconcile(&owner, running(12.4)),
        changed(SENT, Change::Active)
    );
    assert_eq!(reconcile(&owner, running(12.0)), [], "same progress");
    assert_eq!(
        reconcile(&owner, running(8.0)),
        [],
        "progress never goes back"
    );
    let seen = link(&fixture, SENT);
    assert_eq!(
        (&seen["saw_active"], &seen["last_progress"]),
        (&json!(true), &json!(12))
    );

    let finished = report(Complete, FILE);
    let awaiting = changed(SENT, Change::AwaitingVerify);
    assert_eq!(reconcile(&owner, finished.clone()), awaiting);
    assert_eq!(
        outcome(&fixture, SENT),
        (json!("awaiting_verify"), json!("success"))
    );
    let completed_at = link(&fixture, SENT)["completed_at"].clone();
    assert!(completed_at.as_str().is_some_and(|at| !at.is_empty()));
    let stored = links(&fixture, "default");
    let active =
        |link: &&Value| ["watching", "awaiting_verify"].contains(&link["state"].as_str().unwrap());
    let last_active = stored.iter().rfind(active).unwrap();
    assert_eq!(
        last_active["id"], SENT,
        "a patched link moves after the active links"
    );
    assert_eq!(reconcile(&owner, finished.clone()), [], "a repeated finish");
    assert_eq!(links(&fixture, "default"), stored, "a no-op writes nothing");
    assert_eq!(
        links(&fixture, "foreign"),
        foreign,
        "another installation's links stay"
    );
    assert_eq!(
        link(&fixture, "link-other-host")["state"],
        "watching",
        "another host's send stays"
    );
    assert_eq!(
        shown(&owner, SENT),
        Shown::AwaitingVerify,
        "Today offers the bed"
    );

    let rows = confirm_and_deduct(&fixture, &owner, finished);
    assert_eq!(rows.len(), 1, "one deduction for the confirmed bed");
    let row = (rows[0].state, rows[0].link_id.as_str(), rows[0].units);
    assert_eq!(row, (State::Pending, SENT, 2));
    assert_eq!(rows[0].target.as_ref().unwrap().grams, 20.0);
    owner.shutdown().unwrap();
}

#[test]
fn gre_1897_a_cancel_near_the_end_fails_the_link_for_good() {
    let (fixture, owner) = watching(json!({ "saw_active": true, "last_progress": 99 }));
    let cancelled = HostReport {
        cancelled: true,
        ..report(Idle, FILE)
    };
    assert_eq!(
        reconcile(&owner, cancelled),
        changed(SENT, Change::HostFailed)
    );
    assert_eq!(
        outcome(&fixture, SENT),
        (json!("host_failed"), json!("cancelled"))
    );
    assert_eq!(
        reconcile(&owner, report(Complete, FILE)),
        [],
        "no later revival"
    );
    assert_eq!(
        shown(&owner, SENT),
        Shown::HostFailed,
        "Today offers no bed"
    );
    owner.shutdown().unwrap();
}

#[test]
fn gre_1897_an_old_started_send_outlives_an_outage_and_still_finishes() {
    let old = json!({ "created_at": "2020-01-01T00:00:00.000Z", "started": true });
    let (fixture, owner) = watching(old);
    assert_eq!(
        reconcile(&owner, report(Offline, "")),
        [],
        "never dismissed as never run"
    );
    assert_eq!(link(&fixture, SENT)["state"], "watching");
    let awaiting = changed(SENT, Change::AwaitingVerify);
    assert_eq!(reconcile(&owner, report(Complete, FILE)), awaiting);
    owner.shutdown().unwrap();
}

#[test]
fn gre_1897_one_finish_moves_only_the_send_that_printed_it() {
    // `link-verify` was started by its send; `link-upload` is an upload of
    // the same file under another job, started only in the second case.
    let upload = |started: bool| json!({ "id": "link-upload", "upload_job_id": "job-upload", "started": started });
    let sent = json!({ "started": true, "upload_job_id": "job-sent" });
    let (fixture, owner) = watching_with(sent.clone(), &[upload(false)]);
    let printing = HostReport {
        progress: Some(40.0),
        ..report(Printing, FILE)
    };
    assert_eq!(reconcile(&owner, printing), changed(SENT, Change::Active));
    let finished = report(Complete, FILE);
    let awaiting = changed(SENT, Change::AwaitingVerify);
    assert_eq!(reconcile(&owner, finished.clone()), awaiting);
    // The printer keeps showing the finish: a missed poll, then a restart,
    // report it again with `link-upload` the only send still watching.
    assert_eq!(reconcile(&owner, report(Offline, "")), []);
    assert_eq!(
        reconcile(&owner, finished.clone()),
        [],
        "after a missed poll"
    );
    owner.shutdown().unwrap();
    let owner = fixture.open();
    assert_eq!(reconcile(&owner, finished.clone()), [], "after a restart");
    let never_ran = link(&fixture, "link-upload");
    let never_ran = (&never_ran["state"], &never_ran["saw_active"]);
    assert_eq!(never_ran, (&json!("watching"), &json!(false)));
    assert_eq!(
        shown(&owner, "link-upload"),
        Shown::Watching,
        "Today offers no bed for it"
    );
    owner.shutdown().unwrap();

    let (fixture, owner) = watching_with(sent, &[upload(true)]);
    assert_eq!(
        reconcile(&owner, finished),
        [],
        "two started sends of one file: none is guessed"
    );
    for id in [SENT, "link-upload"] {
        assert_eq!(link(&fixture, id)["state"], "watching", "{id}");
    }
    owner.shutdown().unwrap();
}

#[test]
fn gre_1897_only_the_desktop_single_account_policy_reconciles() {
    let (fixture, owner) = watching(json!({}));
    let client = owner.checkoff_reconcile(sessions()).unwrap();
    let refused = client.reconcile(HOST, report(Complete, FILE), WAIT);
    assert!(
        matches!(refused, Err(WriterFailure::Auth(_))),
        "{refused:?}"
    );
    assert_eq!(link(&fixture, SENT)["state"], "watching");
    owner.shutdown().unwrap();
}
