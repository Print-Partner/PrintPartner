//! Adapt the compat log reader without an API-to-supervisor dependency.
use anyhow::{Result, anyhow};
use pp_api::{
    admin::AdminHttpConfig,
    diagnostics::{LogRecord, LogRecords, diagnostics_router},
};
use pp_storage::{WriterOwner, admin::AdminFiles, auth::AuthPolicy};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

struct CoreRecords(PathBuf);
impl LogRecords for CoreRecords {
    fn records(&self) -> std::io::Result<Vec<LogRecord>> {
        pp_compat::CoreLog::diagnostic_records(&self.0).map(|rows| {
            rows.into_iter()
                .map(|(file, retained_file, offset, value)| LogRecord {
                    file,
                    retained_file,
                    offset,
                    value,
                })
                .collect()
        })
    }
}

pub(crate) fn router(
    owner: &Arc<Mutex<WriterOwner>>,
    policy: AuthPolicy,
    files: AdminFiles,
    (origin, account): (&str, &str),
) -> Result<axum::Router> {
    let reads = owner
        .lock()
        .map_err(|_| anyhow!("rebuild owner lock poisoned"))?
        .accepted_reads_with_policy(policy)?;
    Ok(diagnostics_router(
        AdminHttpConfig::from_startup(origin, account, Some("1"))?,
        reads,
        Arc::new(CoreRecords(files.data_directory().to_owned())),
    ))
}
