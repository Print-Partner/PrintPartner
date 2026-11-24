//! Refreshes one observed open draft. The successor row is the durable receipt.
use super::{
    Draft, DraftState, Observations, Origin, PositiveId, WorkingDraftClient, header,
    idempotency_key, insert_snapshot, model, observation, preparation, rebase, save, text,
};
use crate::{WriterFailure, auth, read_model::Credential, write::authorize};
use pp_contracts::autosave::PlanDraftIdentity;
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{sync::Arc, sync::atomic::AtomicBool, time::Duration};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RefreshDraftRequest {
    pub expected_draft: PlanDraftIdentity,
    #[serde(deserialize_with = "Option::deserialize")]
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<u32>")
    )]
    pub expected_header_version: Option<u32>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct RefreshDraftReceipt {
    pub source_draft_id: PositiveId,
    pub successor_draft_id: PositiveId,
}

pub struct RefreshArgs<'a> {
    pub draft_id: PositiveId,
    pub request: RefreshDraftRequest,
    pub idempotency_key: &'a str,
}

#[derive(Debug, thiserror::Error)]
pub enum RefreshError {
    #[error(transparent)]
    Writer(#[from] WriterFailure),
    #[error("invalid refresh observation or idempotency key")]
    InvalidRequest,
    #[error("draft refresh refused")]
    Draft(save::Refusal),
    #[error("the draft has a different author")]
    NotAuthor,
    #[error("another draft is open")]
    CompetingDraft,
    #[error("the draft header changed")]
    HeaderChanged,
    #[error("the draft has saved decisions that refresh cannot carry")]
    UnsupportedDecisions,
    #[error("current Source inputs are unchanged")]
    InputsUnchanged,
    #[error("current Source inputs are not all tracked revisions")]
    UntrackedInputs,
    #[error("preparation would leave no Parts")]
    WouldWipe,
    #[error("saved Part decisions cannot carry")]
    MergeConflicts(Vec<rebase::Conflict>),
}

// The prefix and two hex digests exceed the public 160-unit draft key limit.
const PREFIX: &str = "print-partner:working-drafts:refresh:v1:";

struct Refresh {
    credential: Credential,
    profile: i64,
    draft: i64,
    request: RefreshDraftRequest,
    pattern: String,
    key: String,
    policy: auth::AuthPolicy,
}

enum Checked {
    Existing(RefreshDraftReceipt),
    Open {
        tenant: String,
        actor: String,
        current: Box<Draft>,
    },
}

struct Prepared {
    tenant: String,
    actor: String,
    kit: Option<String>,
    snapshot: preparation::PreparedDraftSnapshot,
}

impl WorkingDraftClient {
    pub fn refresh(
        &self,
        credential: Credential,
        profile: PositiveId,
        args: RefreshArgs<'_>,
        cancelled: Arc<AtomicBool>,
        wait: Duration,
    ) -> Result<RefreshDraftReceipt, RefreshError> {
        let expected = &args.request.expected_draft;
        if expected.draft_id() != args.draft_id
            || *expected.state() != DraftState::Open
            || expected.lifecycle_version().get() >= i32::MAX as u64
        {
            return Err(RefreshError::InvalidRequest);
        }
        let key =
            idempotency_key(args.idempotency_key).map_err(|_| RefreshError::InvalidRequest)?;
        let prefix = format!("{PREFIX}{}:", hex::encode(Sha256::digest(key)));
        let payload = serde_json::to_value(&args.request)
            .map_err(|e| WriterFailure::from(anyhow::Error::from(e)))?;
        let refresh = Refresh {
            credential,
            profile: profile.get() as i64,
            draft: args.draft_id.get() as i64,
            request: args.request,
            pattern: format!("{prefix}*"),
            key: format!("{prefix}{}", model::digest(&payload)),
            policy: self.policy,
        };
        let observations = self.observations.clone().ok_or_else(|| {
            WriterFailure::Internal(anyhow::anyhow!("draft observations are not configured"))
        })?;
        let mut budget = observation::PreparationBudget::new(&cancelled, observations.limits)
            .map_err(preparation_error)?;
        let prepared =
            self.reader
                .read("working_drafts.prepare_refresh", &cancelled, wait, |tx| {
                    refresh.prepare(tx, &observations, &mut budget)
                })??;
        let prepared = match prepared {
            Ok(result) => result,
            Err(receipt) => return Ok(receipt),
        };
        let cancel_write = cancelled.clone();
        self.shared
            .write("working_drafts.refresh", &cancelled, wait, move |tx, _| {
                let mut budget =
                    observation::PreparationBudget::new(&cancel_write, observations.limits)
                        .map_err(preparation_error)?;
                refresh.commit(tx, &observations, prepared, &mut budget)
            })?
    }
}

impl Refresh {
    fn prepare(
        &self,
        tx: &Transaction<'_>,
        observations: &Observations,
        budget: &mut observation::PreparationBudget<'_>,
    ) -> Result<Result<Prepared, RefreshDraftReceipt>, RefreshError> {
        let (tenant, actor, current) = match self.check(tx, budget)? {
            Checked::Existing(receipt) => return Ok(Err(receipt)),
            Checked::Open {
                tenant,
                actor,
                current,
            } => (tenant, actor, current),
        };
        let base = model::project(&current.header, &["baseRevisionId", "basePlanVersion"]);
        let mut reader = tx;
        let mut snapshot = preparation::prepare(
            &mut reader,
            &tenant,
            self.profile,
            base,
            preparation::PreparationContext {
                options: &preparation::PreparationOptions {
                    apply_manifest: false,
                    ..Default::default()
                },
                repos: &observations.repos,
            },
            observations.reads.as_ref(),
            budget,
        )
        .map_err(preparation_error)?;
        if snapshot
            .capture
            .inputs
            .iter()
            .any(|input| input["trackingKind"] != "revision")
        {
            return Err(RefreshError::UntrackedInputs);
        }
        if rebase::canonical_inputs(&snapshot.capture.inputs)
            == rebase::canonical_inputs(&current.inputs)
        {
            return Err(RefreshError::InputsUnchanged);
        }
        let conflicts =
            rebase::merge_refresh(tx, &tenant, self.profile, &current, &mut snapshot, budget)
                .map_err(preparation_error)?;
        if !conflicts.is_empty() {
            return Err(RefreshError::MergeConflicts(conflicts));
        }
        let kit = self.kit(tx, &tenant)?;
        Ok(Ok(Prepared {
            tenant,
            actor,
            kit,
            snapshot,
        }))
    }

    fn commit(
        &self,
        tx: &Transaction<'_>,
        observations: &Observations,
        prepared: Prepared,
        budget: &mut observation::PreparationBudget<'_>,
    ) -> Result<RefreshDraftReceipt, RefreshError> {
        let (tenant, actor) = match self.check(tx, budget)? {
            Checked::Existing(receipt) => return Ok(receipt),
            Checked::Open { tenant, actor, .. } => (tenant, actor),
        };
        if tenant != prepared.tenant || actor != prepared.actor {
            return Err(RefreshError::NotAuthor);
        }
        let captured = preparation::capture(
            tx,
            &tenant,
            self.profile,
            observations.reads.as_ref(),
            budget,
        )
        .map_err(preparation_error)?;
        if captured.fingerprint != prepared.snapshot.capture.fingerprint
            || self.kit(tx, &tenant)? != prepared.kit
        {
            return Err(RefreshError::Draft(save::Refusal::InputsChanged));
        }
        let expected = &self.request.expected_draft;
        let source = rebase::Source::observed_open(
            self.draft,
            expected.lifecycle_version().get() as i64,
            expected.snapshot_digest().as_str(),
        );
        if !rebase::replace_open(tx, &tenant, self.profile, &source).map_err(WriterFailure::from)? {
            return Err(RefreshError::Draft(save::Refusal::DraftChanged));
        }
        let successor = insert_snapshot(
            tx,
            &tenant,
            &actor,
            self.profile,
            &self.key,
            prepared.snapshot.rows(),
            Origin::Rebased(&source),
        )
        .map_err(WriterFailure::from)?;
        budget.check().map_err(preparation_error)?;
        self.receipt(successor)
    }

    fn kit(&self, tx: &Transaction<'_>, tenant: &str) -> Result<Option<String>, WriterFailure> {
        preparation::setting(tx, tenant, &format!("kit_manifest_{}", self.profile))
            .map_err(WriterFailure::from)
    }

    fn receipt(&self, successor: i64) -> Result<RefreshDraftReceipt, RefreshError> {
        let successor_draft_id = PositiveId::new(successor as u64).map_err(|_| {
            WriterFailure::Internal(anyhow::anyhow!("Invalid refresh successor id"))
        })?;
        Ok(RefreshDraftReceipt {
            source_draft_id: self.request.expected_draft.draft_id(),
            successor_draft_id,
        })
    }

    fn check(
        &self,
        tx: &Transaction<'_>,
        budget: &mut observation::PreparationBudget<'_>,
    ) -> Result<Checked, RefreshError> {
        let installation = authorize(tx, &self.credential, self.policy)?;
        if !matches!(self.credential, Credential::Session(_)) {
            return Err(RefreshError::NotAuthor);
        }
        let (_, actor) =
            save::tenant_actor(tx, &self.credential, self.policy).map_err(WriterFailure::from)?;
        let tenant = installation.id();
        let prior = tx.query_row(
            "SELECT id,idempotency_key FROM plan_drafts WHERE tenant_id=? AND profile_id=? AND created_by=? AND idempotency_key GLOB ?",
            params![tenant, self.profile, actor, self.pattern],
            |row| Ok((row.get(0)?, row.get::<_, String>(1)?)),
        ).optional().map_err(WriterFailure::from)?;
        if let Some((id, key)) = prior {
            return if key == self.key {
                Ok(Checked::Existing(self.receipt(id)?))
            } else {
                Err(RefreshError::Draft(save::Refusal::IdempotencyConflict))
            };
        }
        let plan = super::one(
            tx,
            "SELECT * FROM build_profiles WHERE tenant_id=? AND id=?",
            &[&tenant, &self.profile],
        )
        .map_err(WriterFailure::from)?
        .ok_or(RefreshError::Draft(save::Refusal::NotFound))?;
        let expected = &self.request.expected_draft;
        let base = save::accepted_base(tx, tenant, &plan, expected.base())
            .map_err(WriterFailure::from)?
            .map_err(RefreshError::Draft)?;
        let current = save::observed_draft(tx, tenant, self.profile, &base, Some(expected), budget)
            .map_err(preparation_error)?
            .map_err(RefreshError::Draft)?
            .ok_or(RefreshError::Draft(save::Refusal::DraftChanged))?;
        if text(&current.header, "createdBy").map_err(WriterFailure::from)? != actor {
            return Err(RefreshError::NotAuthor);
        }
        let header = header::read(tx, tenant, self.draft).map_err(WriterFailure::from)?;
        if header.map(|h| h.header_version) != self.request.expected_header_version {
            return Err(RefreshError::HeaderChanged);
        }
        let competing = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM plan_drafts WHERE tenant_id=? AND profile_id=? AND state='open' AND id<>?)",
            params![tenant, self.profile, self.draft], |row| row.get(0),
        ).map_err(WriterFailure::from)?;
        if competing {
            return Err(RefreshError::CompetingDraft);
        }
        // Reconciliation decisions name old draft Part IDs. Other stored
        // overrides outside the rebase merge can originate in imported drafts.
        let unsupported = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM plan_draft_required_unit_decisions c
                JOIN plan_drafts d ON d.current_required_unit_reconciliation_id=c.reconciliation_id
                WHERE d.tenant_id=?1 AND d.id=?2)
              OR EXISTS(SELECT 1 FROM plan_draft_parts p
                LEFT JOIN plan_revision_parts b ON b.id=p.base_revision_part_id AND b.tenant_id=p.tenant_id
                WHERE p.tenant_id=?1 AND p.draft_id=?2 AND (
                    p.role_override IS NOT b.role_override OR p.notes IS NOT COALESCE(b.notes,'')
                    OR (b.id IS NOT NULL AND (
                        p.filament_color_id IS NOT b.filament_color_id
                        OR p.filament_custom_hex IS NOT b.filament_custom_hex
                        OR p.spoolman_spool_id IS NOT b.spoolman_spool_id))))",
            params![tenant, self.draft], |row| row.get(0),
        ).map_err(WriterFailure::from)?;
        if unsupported {
            return Err(RefreshError::UnsupportedDecisions);
        }
        Ok(Checked::Open {
            tenant: tenant.to_owned(),
            actor,
            current: Box::new(current),
        })
    }
}

fn preparation_error(error: impl Into<anyhow::Error>) -> RefreshError {
    let error = error.into();
    match error.downcast_ref::<preparation::Failure>() {
        Some(preparation::Failure::NoLayers) => RefreshError::Draft(save::Refusal::NoLayers),
        Some(preparation::Failure::NoStls) => RefreshError::Draft(save::Refusal::NoStls),
        Some(preparation::Failure::WouldWipe) => RefreshError::WouldWipe,
        _ if matches!(
            error.downcast_ref(),
            Some(observation::ReadFailure::Cancelled)
        ) =>
        {
            WriterFailure::Cancelled.into()
        }
        _ => WriterFailure::from(error).into(),
    }
}
