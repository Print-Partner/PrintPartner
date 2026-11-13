//! The upload route's port onto [`SourceAcquisitions`] (GRE-428).

use crate::source_acquisition::{AcquisitionError, SourceAcquisitions, StreamedRequest};
use pp_api::catalog::{SourceUploads, UploadFailure, UploadKind, UploadParts};
use pp_storage::{
    WriterFailure,
    catalog::Credentials,
    jobs::Credential,
    uploads::{AdmissionLimits, Target},
};
use std::{path::PathBuf, sync::Arc};

const MIB: u64 = 1024 * 1024;

/// Per-import limits the upload journal admits today: 256 MiB of input,
/// 1 GiB prepared, and the reservation the journal requires for them.
pub const UPLOAD_LIMITS: AdmissionLimits = AdmissionLimits {
    max_input_bytes: 256 * MIB,
    max_prepared_bytes: 1024 * MIB,
    reserved_bytes: 256 * MIB + 3 * 1024 * MIB + 16 * MIB,
};

/// The library budget the journal admits imports against: its largest value.
pub const IMPORT_QUOTA: u64 = 64 * 1024 * MIB;

/// Imports that may hold a slot at once: one, as the worker admission runs
/// one Source import at a time and one job per Source. A second upload while
/// one runs is refused as busy (409) before anything is stored, never
/// answered 202 for work no worker could claim.
pub const IMPORT_SLOTS: usize = 1;

/// Free space an upload must leave on the data volume.
pub const FREE_SPACE_RESERVE: u64 = 5 * 1024 * MIB;

/// Uploads into existing Sources through the runtime's one
/// [`SourceAcquisitions`].
pub struct SourceUploadService {
    acquisitions: Arc<SourceAcquisitions>,
    data_dir: PathBuf,
    reserve: u64,
}

impl SourceUploadService {
    /// `reserve` is the free space, in bytes, an upload must leave under
    /// `data_dir`.
    pub fn new(acquisitions: Arc<SourceAcquisitions>, data_dir: PathBuf, reserve: u64) -> Self {
        Self {
            acquisitions,
            data_dir,
            reserve,
        }
    }
}

impl SourceUploads for SourceUploadService {
    fn check_space(&self, bytes: u64) -> Result<(), UploadFailure> {
        let free = fs2::available_space(&self.data_dir).map_err(|error| {
            tracing::warn!(%error, "Free space unavailable");
            UploadFailure::Failed
        })?;
        if free.saturating_sub(bytes) < self.reserve {
            return Err(UploadFailure::InsufficientSpace);
        }
        Ok(())
    }

    fn closing(&self) -> bool {
        self.acquisitions.closing()
    }

    fn upload(
        &self,
        credentials: Credentials,
        source_id: i64,
        kind: UploadKind,
        files: UploadParts,
    ) -> Result<String, UploadFailure> {
        let reply = self
            .acquisitions
            .acquire_streamed(StreamedRequest {
                credential: match credentials {
                    Credentials::Session(secret) => Credential::Session(secret),
                    Credentials::Key { tenant_id, key } => Credential::RoutedKey {
                        tenant: tenant_id,
                        key,
                    },
                },
                operation_key: format!("upload-{}", hex::encode(rand::random::<[u8; 16]>())),
                target: Target::Existing { source_id },
                kind,
                limits: UPLOAD_LIMITS,
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
