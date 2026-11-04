//! Spoolman inventory reads for filament shortfalls, through pp-net's checked
//! HTTP connections: every DNS answer is checked, no proxy, no redirects, a
//! deadline and a body cap. Two GETs per read (`/api/v1/filament` and
//! `/api/v1/spool`); nothing here writes to Spoolman.

use crate::review_observations::FilamentProviderConfig;
use anyhow::{Result, anyhow, ensure};
use pp_api::accepted::InventoryReader;
use pp_net::outbound::{HttpGuard, SystemResolver};
use pp_storage::read_model::stock::{Inventory, SpoolmanIntegration};
use serde_json::Value;
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

const DEADLINE: Duration = Duration::from_secs(8);
/// pp-net's largest body; a bigger inventory reads as unknown.
const MAX_BODY: usize = 2 * 1024 * 1024;

/// The production [`InventoryReader`].
#[derive(Clone)]
pub struct SpoolmanInventory {
    guard: HttpGuard,
}

impl SpoolmanInventory {
    pub fn new() -> Result<Self> {
        Ok(Self::with_guard(HttpGuard::new(
            Arc::new(SystemResolver),
            DEADLINE,
            MAX_BODY,
        )?))
    }

    pub fn with_guard(guard: HttpGuard) -> Self {
        Self { guard }
    }

    pub async fn read_inventory(&self, integration: &SpoolmanIntegration) -> Result<Inventory> {
        let config = FilamentProviderConfig::new(
            &integration.base_url,
            &integration.id,
            integration.api_key.clone(),
        )?;
        let filaments = self.get(&config, "filament").await?;
        let spools = self.get(&config, "spool").await?;
        // A body that is not a list is unreadable, not an empty inventory.
        ensure!(
            [&filaments, &spools]
                .iter()
                .all(|body| crate::review_observations::list(body).is_some()),
            "Spoolman returned no list"
        );
        Ok(crate::review_observations::parse_inventory(
            &filaments, &spools,
        ))
    }

    async fn get(&self, config: &FilamentProviderConfig, resource: &str) -> Result<Value> {
        let url = config.endpoint(resource);
        // Spoolman's proxy convention, as Node's `authHeaders` sends it.
        let bearer = config.api_key().map(|key| format!("Bearer {}", key.trim()));
        let headers: Vec<(&str, &str)> = bearer
            .iter()
            .map(|value| ("authorization", value.as_str()))
            .collect();
        let reply = self
            .guard
            .request("GET", url.as_str(), &headers, None, DEADLINE)
            .await
            .map_err(|error| anyhow!("Spoolman read failed: {error}"))?;
        ensure!((200..300).contains(&reply.status), "Spoolman refused read");
        Ok(serde_json::from_slice(&reply.body)?)
    }
}

impl InventoryReader for SpoolmanInventory {
    fn read(
        &self,
        integration: SpoolmanIntegration,
    ) -> Pin<Box<dyn Future<Output = Option<Inventory>> + Send>> {
        let reader = self.clone();
        Box::pin(async move { reader.read_inventory(&integration).await.ok() })
    }
}
