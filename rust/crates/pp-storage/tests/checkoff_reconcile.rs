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
        farm::{CheckoffResultState, HostState},
    },
    spoolman_deductions::{Deduction, Filter, State},
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

#[path = "common/checkoff_apply.rs"]
mod checkoff_apply;
#[path = "common/checkoff_fixture.rs"]
mod checkoff_fixture;
#[path = "common/js_json.rs"]
mod js_json;

const HOST: &str = "sim-moonraker";
const SESSION: &str = "required-unit-fixture-secret";
const WAIT: Duration = Duration::from_secs(5);
const SPOOLMAN: &str = r#"[{"id":"sm-1","type":"spoolman","name":"Spoolman","config":{"base_url":"http://127.0.0.1:7912","enabled":true},"created_at":"2026-11-01T00:00:00.000Z","updated_at":"2026-11-01T00:00:00.000Z"}]"#;

fn policy() -> auth::AuthPolicy {
    auth::AuthPolicy {
        registration: auth::RegistrationPolicy::Open,
        first_user: auth::FirstUserTenant::NewUser,
        session_tenant: auth::SessionTenantPolicy::AccountTenant,
    }
}

fn session() -> Credential {
    Credential::Session(Secret::new(SESSION.into()))
}

fn links(fixture: &Fixture, tenant: &str) -> Vec<Value> {
    let raw: String = fixture
        .sql()
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id=?1 AND key='printer.checkoff_links'",
            [tenant],
            |row| row.get(0),
        )
        .unwrap();
    js_json::parse(&raw).as_array().unwrap().clone()
}

fn link(fixture: &Fixture, id: &str) -> Value {
    links(fixture, "default")
        .into_iter()
        .find(|link| link["id"] == id)
        .unwrap()
}

/// Rewrites the `default` links with `fields` merged into link `id`, moves
/// the fixture's other `watching` link to host `moon-3`, and adds a second
/// `watching` send of the same file on host `moon-2`.
fn stored(fixture: &Fixture, id: &str, fields: Value) {
    let mut all = links(fixture, "default");
    for link in &mut all {
        if link["id"] == "link-watching" {
            link["integration_id"] = "moon-3".into();
        }
        if link["id"] == id {
            for (key, value) in fields.as_object().unwrap() {
                link[key] = value.clone();
            }
        }
    }
    let mut other = all[0].clone();
    other["id"] = "link-other-host".into();
    other["integration_id"] = "moon-2".into();
    other["state"] = "watching".into();
    all.push(other);
    fixture
        .sql()
        .execute(
            "UPDATE app_settings SET value=?1 WHERE tenant_id='default' AND key='printer.checkoff_links'",
            [serde_json::to_string(&all).unwrap()],
        )
        .unwrap();
}

/// `link-verify` sent recently and still `watching`.
fn watching(fields: Value) -> (Fixture, WriterOwner) {
    let fixture = Fixture::new();
    let mut fields = fields;
    fields["state"] = "watching".into();
    if fields.get("created_at").is_none() {
        fields["created_at"] = auth_now().into();
    }
    for key in [
        "host_outcome",
        "completed_at",
        "applied_at",
        "last_progress",
    ] {
        if fields.get(key).is_none() {
            fields[key] = Value::Null;
        }
    }
    stored(&fixture, "link-verify", fields);
    let owner = fixture.open();
    (fixture, owner)
}

/// Now, in the `toISOString` form Checkoff link times use.
fn auth_now() -> String {
    let now = time::OffsetDateTime::now_utc();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.000Z",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    )
}

fn report(state: HostState, filename: Option<&str>, progress: Option<f64>) -> HostReport {
    HostReport {
        state,
        filename: filename.map(str::to_owned),
        progress,
    }
}

/// The desktop's single-account policy, the only one the watchers act under.
fn desktop() -> auth::AuthPolicy {
    auth::AuthPolicy {
        registration: auth::RegistrationPolicy::FirstAccountOnly,
        first_user: auth::FirstUserTenant::NewUser,
        session_tenant: auth::SessionTenantPolicy::SingleAccountDefault,
    }
}

fn reconcile(owner: &WriterOwner, host: &str, report: HostReport) -> Vec<(String, Change)> {
    owner
        .checkoff_reconcile(desktop())
        .unwrap()
        .reconcile(host, report, WAIT)
        .unwrap()
}

fn changed(id: &str, change: Change) -> Vec<(String, Change)> {
    vec![(id.to_owned(), change)]
}

fn farm_state(owner: &WriterOwner, id: &str) -> CheckoffResultState {
    let farm = owner
        .farm_reads(policy())
        .unwrap()
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
    let body = json!({
        "idempotency_key": "gre-1897",
        "beds": [{ "link_id": "link-verify", "decisions": [confirm(1), confirm(2)] }]
    });
    let verified = apply(owner, Request::VerifyBatch { body: body.clone() });
    assert_eq!(verified["status"], 200, "{verified}");
    assert_eq!(
        reconcile(owner, HOST, finished),
        [],
        "the finish is still showing"
    );
    let replay = apply(owner, Request::VerifyBatch { body });
    assert_eq!(replay["status"], 200, "{replay}");
    owner
        .spoolman_deductions(policy())
        .unwrap()
        .list(session(), Filter::default(), &AtomicBool::new(false), WAIT)
        .unwrap()
}

#[test]
fn gre_1897_a_finished_print_awaits_verify_once_and_verify_batch_deducts_once() {
    let (fixture, owner) = watching(json!({}));
    assert_eq!(
        farm_state(&owner, "link-verify"),
        CheckoffResultState::Watching
    );
    let before = links(&fixture, "foreign");

    use HostState::*;
    let running = |progress| report(Printing, Some("plate-a.gcode"), Some(progress));
    assert_eq!(
        reconcile(&owner, HOST, running(12.4)),
        changed("link-verify", Change::Active)
    );
    assert_eq!(reconcile(&owner, HOST, running(12.0)), [], "same progress");
    assert_eq!(
        reconcile(&owner, HOST, running(8.0)),
        [],
        "progress never goes back"
    );
    assert_eq!(
        (
            link(&fixture, "link-verify")["saw_active"].clone(),
            link(&fixture, "link-verify")["last_progress"].clone()
        ),
        (json!(true), json!(12))
    );
    let finished = report(Complete, Some("plate-a.gcode"), Some(100.0));
    assert_eq!(
        reconcile(&owner, HOST, finished.clone()),
        changed("link-verify", Change::AwaitingVerify)
    );
    let done = link(&fixture, "link-verify");
    assert_eq!(
        (&done["state"], &done["host_outcome"], &done["saw_active"]),
        (&json!("awaiting_verify"), &json!("success"), &json!(true))
    );
    assert!(
        done["completed_at"]
            .as_str()
            .is_some_and(|at| !at.is_empty())
    );
    let stored = links(&fixture, "default");
    let active: Vec<&Value> = stored
        .iter()
        .filter(|link| ["watching", "awaiting_verify"].contains(&link["state"].as_str().unwrap()))
        .collect();
    assert_eq!(
        active.last().unwrap()["id"],
        "link-verify",
        "a patched link moves after the other active links"
    );
    assert_eq!(
        reconcile(&owner, HOST, finished.clone()),
        [],
        "a repeated finish"
    );
    assert_eq!(links(&fixture, "default"), stored, "a no-op writes nothing");
    assert_eq!(
        links(&fixture, "foreign"),
        before,
        "another installation's links stay"
    );
    let other = link(&fixture, "link-other-host");
    assert_eq!(
        other["state"], "watching",
        "another host's send of the same file stays"
    );

    // Today's beds are the farm read's `awaiting_verify` results.
    assert_eq!(
        farm_state(&owner, "link-verify"),
        CheckoffResultState::AwaitingVerify
    );

    let rows = confirm_and_deduct(&fixture, &owner, finished);
    assert_eq!(rows.len(), 1, "one deduction for the confirmed bed");
    assert_eq!(
        (rows[0].state, rows[0].link_id.as_str(), rows[0].units),
        (State::Pending, "link-verify", 2)
    );
    assert_eq!(rows[0].target.as_ref().unwrap().grams, 20.0);
    owner.shutdown().unwrap();
}

#[test]
fn gre_1897_a_cancelled_or_failed_print_fails_the_link_and_never_awaits_verify() {
    use HostState::*;
    let (fixture, owner) = watching(json!({ "saw_active": true, "last_progress": 40 }));
    assert_eq!(reconcile(&owner, HOST, report(Offline, None, None)), []);
    assert_eq!(
        reconcile(&owner, HOST, report(Idle, None, None)),
        changed("link-verify", Change::HostFailed)
    );
    let failed = link(&fixture, "link-verify");
    assert_eq!(
        (&failed["state"], &failed["host_outcome"]),
        (&json!("host_failed"), &json!("cancelled"))
    );
    assert_eq!(
        reconcile(&owner, HOST, report(Complete, Some("plate-a.gcode"), None)),
        [],
        "a later finish does not revive a failed link"
    );
    assert_eq!(
        farm_state(&owner, "link-verify"),
        CheckoffResultState::HostFailed
    );
    owner.shutdown().unwrap();

    let (fixture, owner) = watching(json!({}));
    assert_eq!(
        reconcile(&owner, HOST, report(Error, Some("PLATE-A.gcode"), None)),
        changed("link-verify", Change::HostFailed)
    );
    assert_eq!(link(&fixture, "link-verify")["host_outcome"], "failed");
    owner.shutdown().unwrap();

    let (fixture, owner) = watching(json!({ "saw_active": true, "last_progress": 99 }));
    assert_eq!(
        reconcile(&owner, HOST, report(Idle, None, None)),
        changed("link-verify", Change::AwaitingVerify),
        "a print seen at 99% whose brief finish was missed"
    );
    assert_eq!(link(&fixture, "link-verify")["host_outcome"], "success");
    owner.shutdown().unwrap();
}

#[test]
fn gre_1897_an_old_send_that_never_ran_is_dismissed_only_while_the_host_is_quiet() {
    use HostState::*;
    let (fixture, owner) = watching(json!({ "created_at": "2020-01-01T00:00:00.000Z" }));
    assert_eq!(
        reconcile(
            &owner,
            HOST,
            report(Printing, Some("other.gcode"), Some(3.0))
        ),
        []
    );
    assert_eq!(
        reconcile(&owner, HOST, report(Idle, None, None)),
        changed("link-verify", Change::Dismissed)
    );
    let dismissed = link(&fixture, "link-verify");
    assert_eq!(
        (&dismissed["state"], &dismissed["host_outcome"]),
        (&json!("dismissed"), &json!("unknown"))
    );
    owner.shutdown().unwrap();
}

#[test]
fn gre_1897_only_the_desktop_single_account_policy_reconciles() {
    let (fixture, owner) = watching(json!({}));
    let refused = owner.checkoff_reconcile(policy()).unwrap().reconcile(
        HOST,
        report(HostState::Complete, Some("plate-a.gcode"), None),
        WAIT,
    );
    assert!(
        matches!(refused, Err(WriterFailure::Auth(_))),
        "{refused:?}"
    );
    assert_eq!(link(&fixture, "link-verify")["state"], "watching");
    owner.shutdown().unwrap();
}
