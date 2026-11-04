//! Rust-only startup admission: the ownership marker decision runs first.
//!
//! [`admit`] opens the storage writer, which takes the directory lease and
//! applies the marker decision (`pp_storage::ownership_marker`) before any
//! schema write. Only an accepted directory reaches the release check
//! (`pp_compat::verify_bundle`), `cutover::recover` and source-sync admission,
//! in that order. A refused directory sees none of the three and stays as it
//! was. A failure after the marker decision shuts the writer down, which
//! releases the lease; the marker and schema stay, because Rust now owns the
//! directory.

use anyhow::{Context, Result};
use pp_gateway::ownership::cutover::{self, OperationOwners};
use pp_storage::WriterOwner;
use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests;

/// An accepted directory after every startup step ran.
pub(crate) struct Admitted<V> {
    pub(crate) writer: WriterOwner,
    pub(crate) verified: V,
    pub(crate) owners: OperationOwners,
}

/// Run the Rust-only startup steps against `data_dir` in this order:
///
/// 1. `open_writer`: lease, marker decision, then schema. A refusal returns
///    here.
/// 2. `verify`: the release check.
/// 3. `cutover::recover` without an ownership index.
/// 4. Source-sync admission.
///
/// `open_writer` and `verify` are parameters so tests can apply an explicit
/// marker gate and skip the measured release.
pub(crate) async fn admit<V: Send + 'static>(
    data_dir: PathBuf,
    open_writer: impl FnOnce(&Path) -> Result<WriterOwner> + Send + 'static,
    verify: impl FnOnce() -> Result<V> + Send + 'static,
) -> Result<Admitted<V>> {
    let writer = tokio::task::spawn_blocking(move || open_writer(&data_dir))
        .await
        .context("Storage open task failed")??;
    // Everything below runs only on a directory the marker decision accepted.
    match after_marker_decision(&writer, verify).await {
        Ok((verified, owners)) => Ok(Admitted {
            writer,
            verified,
            owners,
        }),
        Err(error) => {
            release(writer).await;
            Err(error)
        }
    }
}

async fn after_marker_decision<V: Send + 'static>(
    writer: &WriterOwner,
    verify: impl FnOnce() -> Result<V> + Send + 'static,
) -> Result<(V, OperationOwners)> {
    let data_dir = writer
        .lease()
        .context("Storage lease unavailable")?
        .data_dir()
        .to_owned();
    let verified = tokio::task::spawn_blocking(verify)
        .await
        .context("Release verification task failed")??;
    // Rust-only mode admits no operation cutover: a cutover record refuses
    // startup, and stray cutover temporaries are removed.
    let (owners, boot) = cutover::recover(&data_dir, None)
        .map_err(|error| anyhow::anyhow!("Operation cutover recovery failed: {error:?}"))?;
    if !boot.evidence_saved {
        eprintln!("cutover_evidence_unsaved");
        tracing::warn!("cutover_evidence_unsaved");
    }
    pp_gateway::ownership::boot_bundled_source_sync(&data_dir)
        .map_err(|error| anyhow::anyhow!("Source Sync ownership admission failed: {error}"))?;
    Ok((verified, owners))
}

/// Shut a writer down after a failed startup step. A failed shutdown keeps the
/// lease until process exit (`WriterOwner::shutdown`), so no other writer can
/// open the directory meanwhile.
async fn release(writer: WriterOwner) {
    match tokio::task::spawn_blocking(move || writer.shutdown()).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::warn!(error = %error, "rust_startup_writer_release_failed");
        }
        Err(error) => {
            tracing::warn!(error = %error, "rust_startup_writer_release_task_failed");
        }
    }
}
