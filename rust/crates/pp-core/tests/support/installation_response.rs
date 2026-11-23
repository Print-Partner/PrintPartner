//! Included by the backup, restore, and Build create test binaries.

use super::Server;
use reqwest::Method;
use serde_json::Value;

impl Server {
    pub async fn exchange(&self, method: Method, path: &str) -> (u16, reqwest::Response) {
        let response = self.request(method, path).send().await.unwrap();
        (response.status().as_u16(), response)
    }

    pub async fn json(&self, method: Method, path: &str) -> (u16, Value) {
        let (status, response) = self.exchange(method, path).await;
        (status, response.json().await.unwrap_or(Value::Null))
    }
}
