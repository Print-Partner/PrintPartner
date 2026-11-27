//! Move deferred until after #171.

pub mod build_url;
#[cfg(feature = "desktop_rebuild")]
pub mod checkoff_3mf;
pub mod discovery;
pub mod exports;
pub mod guide;
#[cfg(feature = "desktop_rebuild")]
pub mod kit;
pub mod mcp_domain;
pub mod native_slicers;
pub mod profile_sync;
pub mod sync;
