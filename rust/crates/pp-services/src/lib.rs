//! Move deferred until after #171.

pub mod build_url;
pub mod discovery;
pub mod exports;
#[cfg(feature = "desktop_rebuild")]
pub mod kit;
pub mod mcp_domain;
pub mod native_slicers;
pub mod profile_sync;
pub mod sync;
