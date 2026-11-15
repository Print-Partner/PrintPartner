//! Shared by the admin-gated settings route tests: a writer over `directory`,
//! a loopback listener, and the admin config and clients a router takes.
use pp_api::admin::AdminHttpConfig;
use pp_storage::{Limits, WriterOwner, app_settings::AppSettingsClient, read_model::ReadClient};
use std::path::Path;
use tokio::net::TcpListener;

#[path = "auth_policy.rs"]
mod auth_policy;

pub struct SettingsServer {
    pub owner: WriterOwner,
    pub listener: TcpListener,
    pub origin: String,
    pub config: AdminHttpConfig,
    pub reads: ReadClient,
    pub settings: AppSettingsClient,
}

impl SettingsServer {
    /// `flag` is the `PP_DESKTOP_REBUILD` value the routes see.
    pub async fn open(directory: &Path, flag: Option<&str>) -> Self {
        let owner = WriterOwner::open(directory, Limits::default()).unwrap().0;
        let (listener, origin) = pp_testkit::http_listener().await.unwrap();
        let config = AdminHttpConfig::from_startup(&origin, "default", flag).unwrap();
        let settings = owner
            .app_settings_with_policy(auth_policy::policy())
            .unwrap();
        let reads = owner.accepted_reads().unwrap();
        Self {
            owner,
            listener,
            origin,
            config,
            reads,
            settings,
        }
    }
}
