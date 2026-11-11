pub mod auth;
#[cfg(feature = "desktop_rebuild")]
pub mod problem;
pub mod serving;
/// With `desktop_rebuild` off, pp-api errors keep their legacy JSON bodies.
#[cfg(not(feature = "desktop_rebuild"))]
pub mod problem {
    /// A legacy body has no problem envelope, so it comes back unchanged.
    pub fn without_envelope(body: serde_json::Value) -> serde_json::Value {
        body
    }
}

#[cfg(feature = "desktop_rebuild")]
pub mod accepted;
#[cfg(feature = "desktop_rebuild")]
pub mod admin;
#[cfg(feature = "desktop_rebuild")]
pub mod builds;
#[cfg(feature = "desktop_rebuild")]
pub mod catalog;
#[cfg(feature = "desktop_rebuild")]
pub mod checkoff;
#[cfg(feature = "desktop_rebuild")]
pub mod client_contract;
#[cfg(feature = "desktop_rebuild")]
pub mod drafts;
#[cfg(feature = "desktop_rebuild")]
pub mod events;
#[cfg(feature = "desktop_rebuild")]
pub mod farm;
#[cfg(feature = "desktop_rebuild")]
pub mod file_assignments;
#[cfg(feature = "desktop_rebuild")]
pub mod fixtures;
#[cfg(feature = "desktop_rebuild")]
pub mod history;
#[cfg(feature = "desktop_rebuild")]
pub mod jobs;
#[cfg(feature = "desktop_rebuild")]
mod loopback;
#[cfg(feature = "desktop_rebuild")]
pub mod mcp;
#[cfg(feature = "desktop_rebuild")]
pub mod modules;
#[cfg(feature = "desktop_rebuild")]
mod multipart_form;
#[cfg(feature = "desktop_rebuild")]
pub mod planning;
#[cfg(feature = "desktop_rebuild")]
pub mod plates;
#[cfg(feature = "desktop_rebuild")]
pub mod printer_host;
#[cfg(feature = "desktop_rebuild")]
pub mod printer_status;
#[cfg(feature = "desktop_rebuild")]
pub mod printer_stream;
#[cfg(feature = "desktop_rebuild")]
pub mod send_queue;
#[cfg(feature = "desktop_rebuild")]
pub mod settings;
#[cfg(feature = "desktop_rebuild")]
pub mod workflow;

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
