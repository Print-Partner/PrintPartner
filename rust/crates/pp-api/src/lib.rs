pub mod auth;

#[cfg(feature = "desktop_rebuild")]
pub mod accepted;
#[cfg(feature = "desktop_rebuild")]
pub mod admin;
#[cfg(feature = "desktop_rebuild")]
pub mod backup;
#[cfg(feature = "desktop_rebuild")]
pub mod builds;
#[cfg(feature = "desktop_rebuild")]
pub mod catalog;
#[cfg(feature = "desktop_rebuild")]
pub mod client_contract;
#[cfg(feature = "desktop_rebuild")]
pub mod drafts;
#[cfg(feature = "desktop_rebuild")]
pub mod fixtures;
#[cfg(feature = "desktop_rebuild")]
pub mod jobs;
#[cfg(feature = "desktop_rebuild")]
mod loopback;
#[cfg(feature = "desktop_rebuild")]
pub mod mcp;

#[cfg(test)]
#[path = "../../pp-storage/tests/common/cleanup.rs"]
mod cleanup;
/// Auto-removing test scratch, shared with pp-storage's tests.
#[cfg(test)]
#[path = "../../pp-storage/tests/common/scratch.rs"]
mod scratch;

#[cfg(test)]
#[test]
fn scratch_root_is_empty_after_lib_tests() {
    cleanup::assert_test_run_clean(&["--skip", "scratch_root_is_empty_after_lib_tests"]);
}
