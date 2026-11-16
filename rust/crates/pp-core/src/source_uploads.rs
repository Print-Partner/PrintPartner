//! The upload route's port onto [`SourceAcquisitions`] (GRE-428, GRE-1445).
//!
//! An upload's input and prepared budgets come from its `Content-Length` and
//! the free space left after the reserve. The journal still requires a
//! reservation of input + 3 × prepared + 16 MiB, and that reservation must
//! fit the quota row's `i64`.

use crate::source_acquisition::{AcquisitionError, SourceAcquisitions, StreamedRequest};
use pp_api::catalog::{SourceUploads, UploadFailure, UploadKind, UploadParts};
use pp_storage::{
    WriterFailure,
    app_settings::{self, AppSettingsClient, Key, Outcome},
    auth::Secret,
    catalog::Credentials,
    jobs::Credential,
    read_model::Credential as ReadCredential,
    uploads::{AdmissionLimits, Target},
};
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

const MIB: u64 = 1024 * 1024;
const RESERVATION_OVERHEAD: u64 = 16 * MIB;
/// `source_import_quota.reserved_bytes` is an `i64`.
const MAX_RESERVED: u64 = i64::MAX as u64;
const SETTING_WAIT: Duration = Duration::from_secs(5);

/// No library ceiling. The journal still accounts each reservation, and a
/// sum that overflows `u64` is refused. A test that passes a smaller quota
/// keeps the ceiling it names.
pub const IMPORT_QUOTA: u64 = u64::MAX;

/// Imports that may hold a slot at once: one, as the worker admission runs
/// one Source import at a time and one job per Source. A second upload while
/// one runs is refused as busy (409) before anything is stored, never
/// answered 202 for work no worker could claim.
pub const IMPORT_SLOTS: usize = 1;

/// Uploads into existing Sources through the runtime's one
/// [`SourceAcquisitions`].
pub struct SourceUploadService {
    acquisitions: Arc<SourceAcquisitions>,
    data_dir: PathBuf,
    settings: AppSettingsClient,
}

impl SourceUploadService {
    /// `settings` holds the free-space reserve. A missing or unreadable value
    /// is 5 GiB.
    pub fn new(
        acquisitions: Arc<SourceAcquisitions>,
        data_dir: PathBuf,
        settings: AppSettingsClient,
    ) -> Self {
        Self {
            acquisitions,
            data_dir,
            settings,
        }
    }

    /// The reserve and the budgets for a body of `bytes`, read before any
    /// byte is staged.
    fn admission(
        &self,
        credentials: Credentials,
        bytes: u64,
    ) -> Result<(Credential, AdmissionLimits), UploadFailure> {
        let (reader, admit) = split_credentials(credentials);
        let reserve = self.reserve(reader)?;
        let free = fs2::available_space(&self.data_dir).map_err(|error| {
            tracing::warn!(%error, "Free space unavailable");
            UploadFailure::Failed
        })?;
        Ok((admit, limits_for_upload(bytes, free, reserve)?))
    }

    fn reserve(&self, credential: ReadCredential) -> Result<u64, UploadFailure> {
        let outcome = self
            .settings
            .read(
                credential,
                Key::ImportReserve,
                &AtomicBool::new(false),
                SETTING_WAIT,
            )
            .map_err(storage_failure)?;
        let Outcome::Value(stored) = outcome else {
            return Err(UploadFailure::Failed);
        };
        Ok(app_settings::import_reserve_bytes(stored.as_deref()))
    }
}

impl SourceUploads for SourceUploadService {
    fn check_space(&self, credentials: Credentials, bytes: u64) -> Result<(), UploadFailure> {
        self.admission(credentials, bytes).map(|_| ())
    }

    fn closing(&self) -> bool {
        self.acquisitions.closing()
    }

    fn upload(
        &self,
        credentials: Credentials,
        source_id: i64,
        kind: UploadKind,
        declared_bytes: u64,
        files: UploadParts,
    ) -> Result<String, UploadFailure> {
        let (credential, limits) = self.admission(credentials, declared_bytes)?;
        let reply = self
            .acquisitions
            .acquire_streamed(StreamedRequest {
                credential,
                operation_key: format!("upload-{}", hex::encode(rand::random::<[u8; 16]>())),
                target: Target::Existing { source_id },
                kind,
                limits,
                files,
            })
            .map_err(failure)?;
        match reply.admitted() {
            Ok(operation) => Ok(operation.job_id),
            Err(_) if self.acquisitions.closing() => Err(UploadFailure::Unavailable),
            Err(error) => Err(failure(error)),
        }
    }
}

/// Budgets for one upload. Refuses when `body` would leave less than
/// `reserve` free. The input budget is the declared body (at least 1). The
/// prepared budget is the room left after the body and the reserve, capped
/// so the reservation fits an `i64`.
pub fn limits_for_upload(
    body: u64,
    free: u64,
    reserve: u64,
) -> Result<AdmissionLimits, UploadFailure> {
    if free.saturating_sub(body) < reserve {
        return Err(UploadFailure::InsufficientSpace);
    }
    let max_input_bytes = body.max(1);
    let room = free.saturating_sub(reserve).saturating_sub(body).max(1);
    let headroom = MAX_RESERVED
        .saturating_sub(max_input_bytes)
        .saturating_sub(RESERVATION_OVERHEAD);
    let max_prepared_bytes = room.min(headroom / 3).max(1);
    let reserved_bytes = max_prepared_bytes
        .checked_mul(3)
        .and_then(|prepared| prepared.checked_add(max_input_bytes))
        .and_then(|total| total.checked_add(RESERVATION_OVERHEAD))
        .filter(|reserved| *reserved <= MAX_RESERVED)
        .ok_or(UploadFailure::Failed)?;
    Ok(AdmissionLimits {
        reserved_bytes,
        max_input_bytes,
        max_prepared_bytes,
    })
}

/// The settings read and the import admission each need their own credential.
/// `Secret` is not `Clone`, so both are built from the same exposed text.
fn split_credentials(credentials: Credentials) -> (ReadCredential, Credential) {
    match credentials {
        Credentials::Session(secret) => {
            let text = secret.expose().to_owned();
            (
                ReadCredential::Session(Secret::new(text.clone())),
                Credential::Session(Secret::new(text)),
            )
        }
        Credentials::Key { tenant_id, key } => {
            let text = key.expose().to_owned();
            (
                ReadCredential::ApiKey {
                    routed_tenant: tenant_id.clone(),
                    secret: Secret::new(text.clone()),
                },
                Credential::RoutedKey {
                    tenant: tenant_id,
                    key: Secret::new(text),
                },
            )
        }
    }
}

/// A stopped or full writer is the same failure the import admission
/// reports. `AppSettingsClient` returns those as text.
fn storage_failure(error: anyhow::Error) -> UploadFailure {
    tracing::warn!(
        error = %format!("{error:#}"),
        "Import reserve setting could not be read"
    );
    let failure = match error.to_string().as_str() {
        "Storage stopped" | "Writer admission poisoned" => WriterFailure::Stopped,
        "Writer queue full" => WriterFailure::QueueFull,
        "Cancelled before admission" => WriterFailure::Cancelled,
        _ => WriterFailure::from(error),
    };
    UploadFailure::Storage(failure)
}

fn failure(error: AcquisitionError) -> UploadFailure {
    match error {
        AcquisitionError::Closed(_) => UploadFailure::Unavailable,
        AcquisitionError::Busy | AcquisitionError::InProgress => UploadFailure::Busy,
        AcquisitionError::NoFiles => UploadFailure::NoFiles,
        AcquisitionError::TooLarge => UploadFailure::TooLarge,
        AcquisitionError::Input(text) => UploadFailure::InvalidFile(text),
        AcquisitionError::GateUnavailable => {
            tracing::warn!("Source upload gate poisoned");
            UploadFailure::Failed
        }
        AcquisitionError::Refused(failure) => {
            if let WriterFailure::Internal(error) = &failure {
                tracing::warn!(error = %format!("{error:#}"), "Source upload storage failure");
            }
            UploadFailure::Storage(failure)
        }
        AcquisitionError::Failed(error) => {
            tracing::warn!(error = %format!("{error:#}"), "Source upload failed");
            UploadFailure::Failed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MIB, limits_for_upload};
    use pp_api::catalog::UploadFailure;

    #[test]
    fn limits_follow_the_body_and_leave_the_reserve_free() {
        let reserve = 5 * 1024 * MIB;
        let limits = limits_for_upload(1000, 10 * 1024 * MIB, reserve).unwrap();
        assert_eq!(limits.max_input_bytes, 1000);
        assert!(limits.max_prepared_bytes > 1024 * MIB);
        let tight = limits_for_upload(1000, reserve + 1000, reserve).unwrap();
        assert_eq!(tight.max_input_bytes, 1000);
        assert_eq!(tight.max_prepared_bytes, 1);
        assert!(matches!(
            limits_for_upload(1000, reserve, reserve),
            Err(UploadFailure::InsufficientSpace)
        ));
    }
}
