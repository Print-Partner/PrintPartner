use super::{BuildCommand, BuildGraphClient, BuildOutcome, Failure, credential_tenant};
use crate::manifest_options::{
    KitManifest, ManifestInputDetail, ManifestJsonBody,
    observation::{self, CapturedManifestGraph, ObservedBuilder},
};
use crate::working_drafts::observation::{PreparationBudget, ReadFailure};
use crate::{auth, read_model};
use anyhow::{Result, anyhow, ensure};
use pp_domain::PositiveId;
use rusqlite::{OptionalExtension, Transaction, params};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

#[derive(Clone, Debug)]
pub enum ManifestOptionsCommand {
    ReadKit { build: PositiveId },
    SaveKit { build: PositiveId, request: Vec<u8> },
    ReadBuilder { build: PositiveId },
}
impl ManifestOptionsCommand {
    fn build(&self) -> i64 {
        match self {
            Self::ReadKit { build } | Self::SaveKit { build, .. } | Self::ReadBuilder { build } => {
                build.get() as i64
            }
        }
    }
    pub(super) fn is_read(&self) -> bool {
        matches!(self, Self::ReadKit { .. })
    }
}

#[derive(Debug)]
pub enum ManifestOptionsOutcome {
    Kit {
        profile_id: i64,
        kit: Box<KitManifest>,
    },
    Saved {
        profile_id: i64,
        kit: Box<KitManifest>,
    },
    Builder(ManifestJsonBody),
    MissingBuild,
    InvalidInput {
        detail: ManifestInputDetail,
    },
    StaleObservation,
    ObservationFailed {
        detail: String,
    },
}
#[derive(Debug)]
pub enum ManifestOptionsFailure {
    MissingBuild,
    InvalidInput { detail: ManifestInputDetail },
    StaleObservation,
    ObservationFailed { detail: String },
}
impl ManifestOptionsOutcome {
    pub fn into_success_body(self) -> Result<ManifestJsonBody, ManifestOptionsFailure> {
        match self {
            Self::Kit { profile_id, kit } | Self::Saved { profile_id, kit } => {
                let mut output = format!("{{\"profile_id\":{profile_id},\"kit\":").into_bytes();
                kit.write_json(&mut output);
                output.push(b'}');
                Ok(ManifestJsonBody(output))
            }
            Self::Builder(body) => Ok(body),
            Self::MissingBuild => Err(ManifestOptionsFailure::MissingBuild),
            Self::InvalidInput { detail } => Err(ManifestOptionsFailure::InvalidInput { detail }),
            Self::StaleObservation => Err(ManifestOptionsFailure::StaleObservation),
            Self::ObservationFailed { detail } => {
                Err(ManifestOptionsFailure::ObservationFailed { detail })
            }
        }
    }
}

pub(super) struct PreparedManifestCommand {
    tenant: String,
    build: i64,
    captured: CapturedManifestGraph,
    observed: ObservedBuilder,
    kit: Option<KitManifest>,
}
impl BuildGraphClient {
    pub fn plan_option_groups(
        &self,
        credential: read_model::Credential,
        build: PositiveId,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<ManifestOptionsOutcome, crate::WriterFailure> {
        let build = build.get() as i64;
        let read = || {
            self.settings
                .read("manifest_options.groups", cancelled, wait, |tx| {
                    let tenant = crate::write::authorize(tx, &credential, self.policy)?;
                    observation::capture(tx, tenant.id(), build).map_err(crate::WriterFailure::from)
                })?
        };
        for _ in 0..2 {
            let Some(captured) = read()? else {
                return Ok(ManifestOptionsOutcome::MissingBuild);
            };
            let result = (|| {
                let mut budget = PreparationBudget::new(cancelled, self.manifest_limits)?;
                let first =
                    observation::observe(&captured, self.manifest_reads.as_deref(), &mut budget)?;
                let second =
                    observation::observe(&captured, self.manifest_reads.as_deref(), &mut budget)?;
                Ok::<_, ReadFailure>((first, second))
            })();
            let (first, second) = match result {
                Ok(pair) => pair,
                Err(ReadFailure::Cancelled) => return Err(crate::WriterFailure::Cancelled),
                Err(error) => {
                    return Ok(ManifestOptionsOutcome::ObservationFailed {
                        detail: error.to_string(),
                    });
                }
            };
            if first.stamps == second.stamps && read()?.as_ref() == Some(&captured) {
                let stored = captured.stored_selections();
                let mut body = Vec::new();
                first.builder.write_option_groups(&stored, &mut body);
                return Ok(ManifestOptionsOutcome::Builder(ManifestJsonBody(body)));
            }
        }
        Ok(ManifestOptionsOutcome::StaleObservation)
    }

    pub(super) fn execute_observed_manifest(
        &self,
        credential: read_model::Credential,
        command: ManifestOptionsCommand,
        cancelled: Arc<AtomicBool>,
        wait: Duration,
    ) -> Result<BuildOutcome> {
        let retries = usize::from(matches!(
            command,
            ManifestOptionsCommand::ReadBuilder { .. }
        ));
        for attempt in 0..=retries {
            ensure!(!cancelled.load(Ordering::Acquire), Failure::Cancelled);
            ensure!(
                !self
                    .shared
                    .queue
                    .lock()
                    .map_err(|_| anyhow!(Failure::Stopped))?
                    .closed,
                Failure::Stopped
            );
            let capture = {
                let mut checkout = self.settings.reader(wait)?;
                let tx = checkout
                    .connection
                    .as_mut()
                    .ok_or_else(|| anyhow!("Reader checkout is closed"))?
                    .transaction()?;
                let tenant = credential_tenant(&tx, &credential, self.policy)?;
                let captured = observation::capture(&tx, &tenant, command.build())?;
                tx.commit()?;
                captured.map(|captured| (tenant, captured))
            };
            let Some((tenant, captured)) = capture else {
                return Ok(BuildOutcome::Manifest(ManifestOptionsOutcome::MissingBuild));
            };
            let kit = match &command {
                ManifestOptionsCommand::SaveKit { request, .. } => {
                    match KitManifest::from_request(request) {
                        Ok(kit) => Some(kit),
                        Err(detail) => {
                            return Ok(BuildOutcome::Manifest(
                                ManifestOptionsOutcome::InvalidInput { detail },
                            ));
                        }
                    }
                }
                _ => None,
            };
            let mut budget = PreparationBudget::new(&cancelled, self.manifest_limits)?;
            let mut observe = || -> std::result::Result<_, ReadFailure> {
                let first =
                    observation::observe(&captured, self.manifest_reads.as_deref(), &mut budget)?;
                let second =
                    observation::observe(&captured, self.manifest_reads.as_deref(), &mut budget)?;
                Ok((first, second))
            };
            let (observed, rechecked) = match observe() {
                Ok(pair) => pair,
                Err(ReadFailure::Cancelled) => return Err(Failure::Cancelled.into()),
                Err(error) => {
                    return Ok(BuildOutcome::Manifest(
                        ManifestOptionsOutcome::ObservationFailed {
                            detail: error.to_string(),
                        },
                    ));
                }
            };
            // Node's loadCommunityManifest warns for each skipped stored slug.
            for source in &observed.invalid_community_slugs {
                eprintln!("Ignoring invalid stored community manifest slug source_id={source}");
            }
            if observed.stamps != rechecked.stamps {
                if attempt < retries {
                    continue;
                }
                return Ok(BuildOutcome::Manifest(
                    ManifestOptionsOutcome::StaleObservation,
                ));
            }
            let prepared = PreparedManifestCommand {
                tenant,
                build: command.build(),
                captured,
                observed,
                kit,
            };
            let outcome = self.execute_with_reply(
                duplicate_credential(&credential),
                BuildCommand::Manifest(command.clone()),
                Some(prepared),
                cancelled.clone(),
                wait,
                false,
            )?;
            if attempt < retries
                && matches!(
                    outcome,
                    BuildOutcome::Manifest(ManifestOptionsOutcome::StaleObservation)
                )
            {
                continue;
            }
            return Ok(outcome);
        }
        unreachable!("bounded manifest observation returns an outcome")
    }
}
fn duplicate_credential(credential: &read_model::Credential) -> read_model::Credential {
    match credential {
        read_model::Credential::Session(secret) => {
            read_model::Credential::Session(auth::Secret::new(secret.expose().to_owned()))
        }
        read_model::Credential::ApiKey {
            routed_tenant,
            secret,
        } => read_model::Credential::ApiKey {
            routed_tenant: routed_tenant.clone(),
            secret: auth::Secret::new(secret.expose().to_owned()),
        },
    }
}

pub(super) fn read(
    tx: &Transaction<'_>,
    tenant: &str,
    command: &ManifestOptionsCommand,
) -> Result<ManifestOptionsOutcome> {
    ensure!(command.is_read(), "Manifest command requires preparation");
    let build = command.build();
    if !super::build_exists(tx, tenant, build)? {
        return Ok(ManifestOptionsOutcome::MissingBuild);
    }
    Ok(ManifestOptionsOutcome::Kit {
        profile_id: build,
        kit: Box::new(read_kit(tx, tenant, build)?),
    })
}

pub(crate) fn read_kit(tx: &Transaction<'_>, tenant: &str, build: i64) -> Result<KitManifest> {
    let raw: Option<String> = tx
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id=?1 AND key=?2",
            params![tenant, format!("kit_manifest_{build}")],
            |row| row.get(0),
        )
        .optional()?;
    Ok(raw
        .and_then(|raw| KitManifest::from_stored(&raw).ok())
        .unwrap_or_default())
}

pub(super) fn finish(
    tx: &Transaction<'_>,
    tenant: &str,
    prepared: PreparedManifestCommand,
) -> Result<ManifestOptionsOutcome> {
    ensure!(
        prepared.tenant == tenant,
        "Authentication principal changed"
    );
    if !observation::basis_matches(tx, &prepared.captured)? {
        return Ok(ManifestOptionsOutcome::StaleObservation);
    }
    let Some(kit) = prepared.kit else {
        let mut output = Vec::new();
        prepared.observed.builder.write_json(&mut output);
        return Ok(ManifestOptionsOutcome::Builder(ManifestJsonBody(output)));
    };
    let groups = &prepared.observed.builder.merged_option_groups;
    if let Err(detail) = observation::validate_known_selections(&kit, groups) {
        return Ok(ManifestOptionsOutcome::InvalidInput { detail });
    }
    for (group_id, variants) in kit.selection_projection() {
        let Some((_, group)) = groups.iter().find(|(id, _)| *id == group_id) else {
            return Ok(ManifestOptionsOutcome::InvalidInput {
                detail: ManifestInputDetail::group(&group_id.0, " is an unknown option group"),
            });
        };
        if variants
            .iter()
            .any(|id| !group.variants.iter().any(|variant| variant.id == *id))
        {
            return Ok(ManifestOptionsOutcome::InvalidInput {
                detail: ManifestInputDetail::group(&group_id.0, " contains an unknown variant id"),
            });
        }
    }
    let mut value = Vec::new();
    kit.write_json(&mut value);
    let value = String::from_utf8(value)?;
    tx.execute("INSERT INTO app_settings(tenant_id,key,value) VALUES(?1,?2,?3) ON CONFLICT(tenant_id,key) DO UPDATE SET value=excluded.value", params![tenant, format!("kit_manifest_{}", prepared.build), value])?;
    super::mark_modified(tx, tenant, prepared.build, &auth::catalog_timestamp())?;
    Ok(ManifestOptionsOutcome::Saved {
        profile_id: prepared.build,
        kit: Box::new(kit),
    })
}
