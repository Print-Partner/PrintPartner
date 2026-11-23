//! Included by every installation HTTP test binary.

use pp_api::admin::AdminHttpConfig;
use pp_core::{
    DesktopRebuild,
    rebuild_api::{desktop_policy, product_router},
};
use pp_storage::WriterOwner;
use reqwest::Method;
use serde_json::json;
use std::{
    any::Any,
    path::PathBuf,
    sync::{Arc, Mutex},
};

pub struct Server {
    pub origin: String,
    pub client: reqwest::Client,
    pub cookie: String,
    serving: Option<pp_api::serving::Serving>,
    pub owner: Arc<Mutex<WriterOwner>>,
    /// The owner's data directory.
    pub data: PathBuf,
    /// The temporary directory that holds `data`, removed on drop.
    root: Box<dyn Any + Send + Sync>,
}

impl Server {
    /// Serves the product routes when `flag` is exactly `1` or `true` and
    /// registers the owner account; otherwise serves the backup router alone,
    /// with no session.
    pub async fn start(
        owner: WriterOwner,
        data: PathBuf,
        root: impl Any + Send + Sync,
        flag: Option<&str>,
    ) -> Self {
        let mut server = Self::serve(owner, data, Box::new(root), flag).await;
        if matches!(flag, Some("1" | "true")) {
            let account = json!({"email": "owner@example.test", "password": "password-1234"});
            let request = server
                .request(Method::POST, "/auth/register")
                .json(&account);
            let signed_in = request.send().await.unwrap();
            assert_eq!(signed_in.status(), 200);
            let set_cookie = signed_in.headers()["set-cookie"].to_str().unwrap();
            server.cookie = pp_testkit::session_cookie(set_cookie);
        }
        server
    }

    pub(super) async fn serve(
        owner: WriterOwner,
        data: PathBuf,
        root: Box<dyn Any + Send + Sync>,
        flag: Option<&str>,
    ) -> Self {
        let owner = Arc::new(Mutex::new(owner));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let on = matches!(flag, Some("1" | "true"));
        let router = if on {
            let rebuild = DesktopRebuild::from_value(flag);
            product_router(rebuild, Arc::clone(&owner), &origin, None)
                .unwrap()
                .expect("flag on mounts the product routes")
        } else {
            let reads = owner.lock().unwrap();
            let reads = reads.accepted_reads_with_policy(desktop_policy()).unwrap();
            let config = AdminHttpConfig::from_startup(&origin, "default", flag).unwrap();
            pp_api::backup::backup_router(config, Arc::clone(&owner), reads)
        };
        Self {
            client: reqwest::Client::builder().no_proxy().build().unwrap(),
            cookie: String::new(),
            serving: Some(pp_api::serving::Serving::start(listener, router)),
            origin,
            owner,
            data,
            root,
        }
    }

    /// A request with this server's Origin and, once signed in, its session.
    pub fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        let request = self
            .client
            .request(method, format!("{}{path}", self.origin))
            .header("Origin", &self.origin);
        if self.cookie.is_empty() {
            request
        } else {
            request.header("Cookie", &self.cookie)
        }
    }

    pub async fn stop(self) {
        self.shutdown().await;
    }

    pub(super) async fn shutdown(mut self) -> (PathBuf, Box<dyn Any + Send + Sync>, String) {
        if let Some(mut serving) = self.serving.take() {
            serving.stop().await.unwrap();
        }
        let owner = Arc::into_inner(self.owner).expect("the router still holds the owner");
        owner.into_inner().unwrap().shutdown().unwrap();
        (self.data, self.root, self.cookie)
    }
}
