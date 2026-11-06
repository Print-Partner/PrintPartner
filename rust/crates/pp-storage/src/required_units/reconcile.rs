use super::{
    Decision, DomainCode, Outcome, ReconciliationCommand,
    model::{self, FORMAT, digest},
    storage::{
        self, Draft, num, one, optional_num, reconciliation_digest, selection_basis_digest, text,
    },
    view::workspace,
};
use crate::{
    auth::{self, Secret},
    read_model::Credential,
};
use anyhow::{Result, anyhow, ensure};
use rusqlite::{Transaction, params};
use serde_json::{Value, json};

/// Returns the tenant and the actor ID Node's route records: the session's
/// user ID, or `tenant:<id>` for an API key.
fn actor(
    tx: &Transaction<'_>,
    credential: &Credential,
    policy: auth::AuthPolicy,
) -> Result<(String, String)> {
    match credential {
        Credential::Session(secret) => {
            let (tenant, subject) = auth::job_session_actor(tx, secret.expose(), policy)?;
            let user = subject
                .strip_prefix("user:")
                .ok_or_else(|| anyhow!(auth::AuthFailure::SessionRequired))?;
            Ok((tenant, user.to_owned()))
        }
        Credential::ApiKey {
            routed_tenant,
            secret,
        } => {
            let key = Secret::new(secret.expose().to_owned());
            let (tenant, _) = auth::job_key_actor(tx, routed_tenant, key)?;
            let actor = format!("tenant:{tenant}");
            Ok((tenant, actor))
        }
    }
}

struct Scope<'a> {
    tx: &'a Transaction<'a>,
    tenant: &'a str,
    profile: i64,
    draft: i64,
    expected: &'a str,
    profile_row: &'a Value,
}

impl Scope<'_> {
    fn ready(&self, d: &Draft) -> Result<Outcome> {
        Ok(Outcome::Ready {
            workspace: workspace(self.tx, self.tenant, self.profile_row, d)?,
        })
    }

    fn select(&self, reconciliation: i64, next: &str, failure: &str) -> Result<Draft> {
        let changed = self.tx.execute(
            "UPDATE plan_drafts SET current_required_unit_reconciliation_id=?,digest_format='plan-draft-v2',snapshot_digest=? WHERE tenant_id=? AND profile_id=? AND id=? AND state='open' AND snapshot_digest=?",
            params![reconciliation, next, self.tenant, self.profile, self.draft, self.expected],
        )?;
        ensure!(
            changed == 1,
            "Required-unit reconciliation selection failed"
        );
        storage::draft(self.tx, self.tenant, self.profile, self.draft)?
            .filter(|d| {
                d.header["snapshotDigest"] == next
                    && d.header["currentRequiredUnitReconciliationId"] == reconciliation
            })
            .ok_or_else(|| anyhow!("{failure}"))
    }

    /// Replays an idempotent request. A v1 draft whose prior selection was
    /// detached gets that reconciliation selected again, as Node repairs it;
    /// the repair writes no domain event.
    fn replay(&self, existing: &Value, payload: &str) -> Result<Outcome> {
        if existing["payloadDigest"] != payload {
            return Ok(Outcome::IdempotencyConflict);
        }
        let reconciliation = num(existing, "id")?;
        let Some(d) = storage::draft(self.tx, self.tenant, self.profile, self.draft)? else {
            return Ok(Outcome::DraftNotFound);
        };
        let selected = &d.header["currentRequiredUnitReconciliationId"];
        if *selected == reconciliation {
            return self.ready(&d);
        }
        if selected.is_null()
            && d.header["state"] == "open"
            && d.header["digestFormat"] == "plan-draft-v1"
            && d.header["snapshotDigest"] == self.expected
        {
            let prior = storage::saved(
                self.tx,
                self.tenant,
                self.profile,
                self.draft,
                reconciliation,
            )?;
            if prior.header["planningDigest"] == d.planning
                && d.planning == self.expected
                && prior.header["baseRevisionId"] == d.header["baseRevisionId"]
            {
                let next = model::selection(
                    &d.planning,
                    Some(text(&prior.header, "reconciliationDigest")?),
                );
                let repaired = self.select(
                    reconciliation,
                    &next,
                    "Required-unit reconciliation repair could not be verified",
                )?;
                return self.ready(&repaired);
            }
        }
        Ok(Outcome::DraftChanged { workspace: None })
    }

    fn reconcile(
        &self,
        command: &ReconciliationCommand,
        actor: &str,
        decision_digest: &str,
        payload: &str,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Outcome> {
        let Some(d) = storage::draft(self.tx, self.tenant, self.profile, self.draft)? else {
            return Ok(Outcome::DraftNotFound);
        };
        if d.header["state"] != "open" {
            return Ok(Outcome::NotOpen {
                workspace: workspace(self.tx, self.tenant, self.profile_row, &d)?,
            });
        }
        if d.header["snapshotDigest"] != self.expected {
            return Ok(Outcome::DraftChanged {
                workspace: Some(workspace(self.tx, self.tenant, self.profile_row, &d)?),
            });
        }
        let revision = optional_num(self.profile_row, "acceptedPlanRevisionId")?;
        let version = num(self.profile_row, "acceptedPlanVersion")?;
        let baseline_required = match revision {
            None => {
                version != 0
                    || one(
                        self.tx,
                        "SELECT id FROM parts WHERE tenant_id=? AND profile_id=? LIMIT 1",
                        &[&self.tenant, &self.profile],
                    )?
                    .is_some()
            }
            Some(_) => version <= 0,
        };
        if baseline_required {
            return Ok(Outcome::AcceptedBaselineRequired);
        }
        if revision != optional_num(&d.header, "baseRevisionId")?
            || version != num(&d.header, "basePlanVersion")?
        {
            return Ok(Outcome::BaseChanged {
                workspace: workspace(self.tx, self.tenant, self.profile_row, &d)?,
            });
        }
        let Some((mapping, base)) = storage::base(self.tx, self.tenant, self.profile, revision)?
        else {
            return Ok(Outcome::DomainError {
                code: DomainCode::RequiredUnitSetUnavailable,
            });
        };
        let result = model::reconcile_checked(
            &d.parts,
            &d.inputs,
            &base,
            &command.request.decisions,
            check,
        )?;
        let basis_digest = selection_basis_digest(&mapping, &result.basis);
        let result_digest = digest(&result.full());
        let reconciliation_digest = reconciliation_digest(&json!({
            "baseRevisionId":revision,
            "baseMappingDigest":mapping,
            "planningDigest":d.planning,
            "selectionBasisDigest":basis_digest,
            "decisionDigest":decision_digest,
            "resultKind":result.result["kind"],
            "resultDigest":result_digest
        }));
        let expected_count = d
            .parts
            .iter()
            .map(|p| num(p, "quantityEffective"))
            .sum::<Result<i64>>()?;
        let now = auth::catalog_timestamp();
        self.tx.execute(
            "INSERT INTO plan_draft_required_unit_reconciliations (tenant_id,profile_id,draft_id,format,planning_digest,base_revision_id,base_mapping_digest,selection_basis_digest,selection_basis_json,decision_digest,result_kind,result_digest,result_json,reconciliation_digest,expected_assignment_count,actor_id,idempotency_key,payload_digest,created_at,finalized_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,NULL)",
            params![
                self.tenant,
                self.profile,
                self.draft,
                FORMAT,
                d.planning,
                revision,
                mapping.as_str(),
                basis_digest,
                result.basis.to_string(),
                decision_digest,
                text(&result.result, "kind")?,
                result_digest,
                result.result.to_string(),
                reconciliation_digest,
                expected_count,
                actor,
                command.idempotency_key,
                payload,
                now
            ],
        )?;
        let reconciliation = self.tx.last_insert_rowid();
        for decision in &command.request.decisions {
            check()?;
            let (kind, predecessor) = match decision {
                Decision::Replace { .. } => ("replace", None),
                Decision::SelectExactPredecessor {
                    predecessor_revision_part_id,
                    ..
                } => (
                    "select_exact_predecessor",
                    Some(predecessor_revision_part_id),
                ),
                Decision::AcceptPriorCompletion {
                    predecessor_revision_part_id,
                    ..
                } => (
                    "accept_prior_completion",
                    Some(predecessor_revision_part_id),
                ),
            };
            self.tx.execute(
                "INSERT INTO plan_draft_required_unit_decisions (tenant_id,reconciliation_id,target_draft_part_id,kind,predecessor_revision_part_id) VALUES (?,?,?,?,?)",
                params![
                    self.tenant,
                    reconciliation,
                    i64::try_from(decision.target().get())?,
                    kind,
                    predecessor.map(|id| i64::try_from(id.get())).transpose()?
                ],
            )?;
        }
        if let Some(assignments) = result.result["assignments"].as_array() {
            for a in assignments {
                check()?;
                self.tx.execute(
                    "INSERT INTO plan_draft_required_unit_assignments (tenant_id,reconciliation_id,target_draft_part_id,unit_index,kind,required_unit_token) VALUES (?,?,?,?,?,?)",
                    params![
                        self.tenant,
                        reconciliation,
                        num(a, "draftPartId")?,
                        num(a, "unitIndex")?,
                        text(a, "kind")?,
                        a["token"].as_str()
                    ],
                )?;
            }
        }
        ensure!(
            self.tx.execute(
                "UPDATE plan_draft_required_unit_reconciliations SET finalized_at=? WHERE tenant_id=? AND id=? AND finalized_at IS NULL",
                params![now, self.tenant, reconciliation],
            )? == 1,
            "Required-unit reconciliation could not be finalized"
        );
        let stored = storage::saved(
            self.tx,
            self.tenant,
            self.profile,
            self.draft,
            reconciliation,
        )?;
        ensure!(
            stored.header["selectionBasisDigest"] == basis_digest
                && stored.header["resultDigest"] == result_digest
                && stored.header["reconciliationDigest"] == reconciliation_digest,
            "Required-unit reconciliation persisted result mismatch"
        );
        let next = model::selection(&d.planning, Some(&reconciliation_digest));
        let selected = self.select(
            reconciliation,
            &next,
            "Required-unit reconciliation selection could not be verified",
        )?;
        crate::domain_events::append(
            self.tx,
            self.tenant,
            crate::domain_events::Change {
                kind: crate::domain_events::DRAFT_CHANGED,
                aggregate_type: "build",
                aggregate_id: self.profile.to_string(),
                aggregate_version: None,
                payload: json!({"draft_id": self.draft}),
            },
        )?;
        self.ready(&selected)
    }
}

/// Saves and selects a Required-unit reconciliation inside the caller's
/// write transaction, as Node's `savePlanDraftRequiredUnitReconciliation` does.
pub(crate) fn run(
    tx: &Transaction<'_>,
    command: &ReconciliationCommand,
    policy: auth::AuthPolicy,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<Outcome> {
    let (tenant, actor) = actor(tx, &command.credential, policy)?;
    let profile = i64::try_from(command.profile_id.get())?;
    let draft = i64::try_from(command.draft_id.get())?;
    let Some(profile_row) = one(
        tx,
        "SELECT accepted_plan_revision_id,accepted_plan_version FROM build_profiles WHERE tenant_id=? AND id=?",
        &[&tenant, &profile],
    )?
    else {
        return Ok(Outcome::ProfileNotFound);
    };
    let expected = command.request.expected_snapshot_digest.as_str();
    let decision_digest = digest(&json!(model::decisions(&command.request.decisions)?));
    let payload = digest(&json!({
        "profile_id":profile,
        "draft_id":draft,
        "expected_snapshot_digest":expected,
        "decision_digest":decision_digest
    }));
    let scope = Scope {
        tx,
        tenant: &tenant,
        profile,
        draft,
        expected,
        profile_row: &profile_row,
    };
    match one(
        tx,
        "SELECT id,payload_digest FROM plan_draft_required_unit_reconciliations WHERE tenant_id=? AND profile_id=? AND draft_id=? AND actor_id=? AND idempotency_key=?",
        &[&tenant, &profile, &draft, &actor, &command.idempotency_key],
    )? {
        Some(existing) => scope.replay(&existing, &payload),
        None => scope.reconcile(command, &actor, &decision_digest, &payload, check),
    }
}
