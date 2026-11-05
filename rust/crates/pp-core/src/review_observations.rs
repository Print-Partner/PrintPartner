use anyhow::{Result, anyhow, ensure};
use pp_source::observation::{Failure as SourceFailure, Inventory, ReadBudget};
use pp_storage::read_model::{
    Artifact, Provenance, Snapshot,
    stock::{Inventory as SpoolmanStock, InventoryFilament, InventorySpool},
    views::{
        FilamentLookup, MediaObservation, ResolvedFilament, ReviewObservations, SpoolSummary,
        catalog_color,
    },
};
use reqwest::{StatusCode, Url, header};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
pub struct ObservationLimits {
    pub entries: usize,
    pub path_bytes: usize,
    pub artifact_bytes: u64,
    pub elapsed: Duration,
}

impl Default for ObservationLimits {
    fn default() -> Self {
        Self {
            entries: pp_source::MAX_ENTRIES,
            path_bytes: 8 * 1024 * 1024,
            artifact_bytes: pp_source::MAX_CONTENT_BYTES,
            elapsed: Duration::from_secs(2),
        }
    }
}

impl ObservationLimits {
    fn validate(self) -> Result<()> {
        let hard = Self::default();
        ensure!(
            self.entries > 0
                && self.entries <= hard.entries
                && self.path_bytes > 0
                && self.path_bytes <= hard.path_bytes
                && self.artifact_bytes > 0
                && self.artifact_bytes <= hard.artifact_bytes
                && !self.elapsed.is_zero()
                && self.elapsed <= hard.elapsed,
            "Invalid Review observation limits"
        );
        Ok(())
    }
}

#[derive(Clone)]
pub struct FilamentProviderConfig {
    base_url: Url,
    integration_id: String,
    api_key: Option<String>,
    timeout: Duration,
    max_response_bytes: usize,
    max_redirects: usize,
}

impl FilamentProviderConfig {
    pub fn new(base_url: &str, integration_id: &str, api_key: Option<String>) -> Result<Self> {
        let mut base_url = Url::parse(base_url)?;
        ensure!(
            matches!(base_url.scheme(), "http" | "https")
                && base_url.host_str().is_some()
                && base_url.username().is_empty()
                && base_url.password().is_none()
                && base_url.query().is_none()
                && base_url.fragment().is_none(),
            "Invalid filament provider URL"
        );
        if let Some(host) = base_url.host_str()
            && let Ok(ip) = host.trim_start_matches('[').trim_end_matches(']').parse()
        {
            ensure!(
                pp_net::outbound::permitted(ip),
                "Prohibited filament provider address"
            );
        }
        let path = base_url.path().trim_end_matches('/').to_owned();
        base_url.set_path(path.strip_suffix("/api/v1").unwrap_or(&path));
        ensure!(
            !integration_id.is_empty()
                && integration_id.len() <= 200
                && !integration_id.contains([':', '\0']),
            "Invalid filament provider identity"
        );
        if let Some(value) = &api_key {
            ensure!(
                !value.trim().is_empty() && value.len() <= 4096,
                "Invalid filament provider credential"
            );
        }
        Ok(Self {
            base_url,
            integration_id: integration_id.into(),
            api_key,
            timeout: Duration::from_secs(8),
            max_response_bytes: 16 * 1024 * 1024,
            max_redirects: 5,
        })
    }

    pub fn with_limits(
        mut self,
        timeout: Duration,
        max_response_bytes: usize,
        max_redirects: usize,
    ) -> Result<Self> {
        ensure!(
            !timeout.is_zero()
                && timeout <= self.timeout
                && max_response_bytes > 0
                && max_response_bytes <= self.max_response_bytes
                && max_redirects <= self.max_redirects,
            "Filament provider limits may only be reduced"
        );
        self.timeout = timeout;
        self.max_response_bytes = max_response_bytes;
        self.max_redirects = max_redirects;
        Ok(self)
    }

    pub(crate) fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }

    pub(crate) fn endpoint(&self, resource: &str) -> Url {
        let path = format!(
            "{}/api/v1/{resource}",
            self.base_url.path().trim_end_matches('/')
        );
        let mut endpoint = self.base_url.clone();
        endpoint.set_path(&path);
        endpoint
    }
}

#[derive(Clone)]
pub struct SnapshotReviewObserver {
    repos_root: PathBuf,
    thumbs_root: Option<PathBuf>,
    limits: ObservationLimits,
    filament: Option<FilamentProviderConfig>,
}

impl SnapshotReviewObserver {
    pub fn new(repos_root: PathBuf, thumbs_root: Option<PathBuf>) -> Result<Self> {
        ensure!(
            repos_root.is_absolute(),
            "Review repositories root must be absolute"
        );
        if let Some(thumbs_root) = &thumbs_root {
            ensure!(
                thumbs_root.is_absolute(),
                "Review thumbnails root must be absolute"
            );
        }
        Ok(Self {
            repos_root,
            thumbs_root,
            limits: ObservationLimits::default(),
            filament: None,
        })
    }

    pub fn with_observation_limits(mut self, limits: ObservationLimits) -> Result<Self> {
        limits.validate()?;
        self.limits = limits;
        Ok(self)
    }

    pub fn with_filament_provider(mut self, provider: FilamentProviderConfig) -> Self {
        self.filament = Some(provider);
        self
    }

    pub fn review_observations(
        &self,
        snapshot: &Snapshot,
        cancelled: &AtomicBool,
    ) -> Result<ReviewObservations> {
        let mut budget = ObservationBudget::new(cancelled, self.limits);
        let mut roots = HashMap::<String, Option<Inventory>>::new();

        let mut available_input_roots = HashSet::new();
        if let Provenance::Tracked { inputs, .. } = &snapshot.provenance {
            for input in inputs {
                budget.check()?;
                if input.tracking_kind == "revision"
                    && let Some(value) = input.snapshot_root.as_deref()
                    && ensure_root(self, &mut roots, value, &mut budget)?.is_some()
                {
                    available_input_roots.insert(input.input_id);
                }
            }
        }

        let mut media_by_part_id = HashMap::new();
        for part in &snapshot.parts {
            budget.check()?;
            if !part.included {
                continue;
            }
            let artifact_missing = match &part.artifact {
                Artifact::Tracked {
                    snapshot_root,
                    relative_path,
                    expected_sha256,
                    ..
                } => artifact_missing(
                    ensure_root(self, &mut roots, snapshot_root, &mut budget)?,
                    relative_path,
                    expected_sha256,
                    &mut budget,
                )?,
                Artifact::Unavailable { .. } => true,
            };
            let thumb_empty = if artifact_missing {
                false
            } else {
                self.thumbnail_missing(part, &mut budget)?
            };
            media_by_part_id.insert(
                part.projection_part_id,
                MediaObservation {
                    artifact_missing,
                    thumb_empty,
                },
            );
        }
        budget.check()?;
        Ok(ReviewObservations {
            available_input_roots,
            media_by_part_id,
        })
    }

    pub async fn filament_lookup(
        &self,
        snapshot: &Snapshot,
        cancelled: Arc<AtomicBool>,
    ) -> Result<impl FilamentLookup + Send + Sync + use<>> {
        ensure!(
            !cancelled.load(Ordering::Acquire),
            "Filament observation cancelled"
        );
        let Some(provider) = &self.filament else {
            return Ok(SnapshotFilamentLookup::default());
        };
        let prefix = format!("spoolman:{}:filament:", provider.integration_id);
        if !snapshot.parts.iter().any(|part| {
            part.filament_color_id
                .as_deref()
                .is_some_and(|value| value.trim().starts_with(&prefix))
        }) {
            return Ok(SnapshotFilamentLookup::default());
        }
        match load_filament(provider, &cancelled).await {
            Ok(value) => Ok(value),
            Err(error) if cancelled.load(Ordering::Acquire) => Err(error),
            Err(_) => Ok(SnapshotFilamentLookup::default()),
        }
    }

    fn snapshot_root(
        &self,
        value: &str,
        budget: &mut ObservationBudget<'_>,
    ) -> Result<Option<PathBuf>> {
        budget.check()?;
        let candidate = Path::new(value);
        if !candidate.is_absolute()
            || candidate
                .components()
                .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
        {
            return Ok(None);
        }
        let configured = match self.repos_root.canonicalize() {
            Ok(value) => value,
            Err(_) => return Ok(None),
        };
        let relative = match candidate.strip_prefix(&self.repos_root) {
            Ok(value) if value.components().next().is_some() => value,
            _ => return Ok(None),
        };
        Ok(Some(configured.join(relative)))
    }

    fn thumbnail_missing(
        &self,
        part: &pp_storage::read_model::Part,
        budget: &mut ObservationBudget<'_>,
    ) -> Result<bool> {
        let Some(root) = &self.thumbs_root else {
            return Ok(true);
        };
        if !fs::symlink_metadata(root)
            .is_ok_and(|value| value.is_dir() && !value.file_type().is_symlink())
        {
            return Ok(true);
        }
        let Artifact::Tracked {
            expected_sha256, ..
        } = &part.artifact
        else {
            return Ok(true);
        };
        if expected_sha256.len() != 64
            || !expected_sha256
                .bytes()
                .all(|value| value.is_ascii_hexdigit())
        {
            return Ok(true);
        }
        let custom = part
            .filament_custom_hex
            .as_deref()
            .map(str::trim)
            .filter(|hex| hex.len() == 7 && hex.starts_with('#') && valid_hex(hex));
        let catalog = part
            .filament_color_id
            .as_deref()
            .and_then(catalog_color)
            .map(|color| color.hex.as_str())
            .filter(|hex| hex.len() == 7 && hex.starts_with('#') && valid_hex(hex));
        let hex = custom
            .or(catalog)
            .map(|hex| format!("#{}", hex.trim_start_matches('#').to_ascii_lowercase()))
            .unwrap_or_default();
        let role = part.effective_role.trim().to_lowercase();
        if role.is_empty() || role.contains('\0') {
            return Ok(true);
        }
        let payload = format!("accepted-thumbnail-v3\0thumbnail\0{expected_sha256}\0{role}\0{hex}");
        let candidate = root.join(format!(
            "{}.png",
            hex::encode(Sha256::digest(payload.as_bytes()))
        ));
        for _ in 0..8 {
            budget.check()?;
            let before = match fs::symlink_metadata(&candidate) {
                Ok(value)
                    if !value.file_type().is_symlink() && value.is_file() && value.len() >= 8 =>
                {
                    value
                }
                _ => return Ok(true),
            };
            let mut file = match fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&candidate)
            {
                Ok(value) => value,
                Err(_) => continue,
            };
            let opened = match file.metadata() {
                Ok(value) if value.is_file() && value.len() == before.len() => value,
                _ => continue,
            };
            let mut signature = [0; 8];
            if file.read_exact(&mut signature).is_err() {
                continue;
            }
            let after = match file.metadata() {
                Ok(value) => value,
                Err(_) => continue,
            };
            if opened.len() == after.len()
                && opened.modified().ok() == after.modified().ok()
                && signature == [137, 80, 78, 71, 13, 10, 26, 10]
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

fn ensure_root<'a>(
    observer: &SnapshotReviewObserver,
    roots: &'a mut HashMap<String, Option<Inventory>>,
    value: &str,
    budget: &mut ObservationBudget<'_>,
) -> Result<&'a mut Option<Inventory>> {
    match roots.entry(value.into()) {
        std::collections::hash_map::Entry::Occupied(entry) => Ok(entry.into_mut()),
        std::collections::hash_map::Entry::Vacant(entry) => {
            let observed = match observer.snapshot_root(value, budget)? {
                Some(path) => Inventory::scan(&path, budget).map_err(source_failure)?,
                None => None,
            };
            Ok(entry.insert(observed))
        }
    }
}

struct ObservationBudget<'a> {
    cancelled: &'a AtomicBool,
    limits: ObservationLimits,
    started: Instant,
    entries: usize,
    path_bytes: usize,
    artifact_bytes: u64,
}

impl<'a> ObservationBudget<'a> {
    fn new(cancelled: &'a AtomicBool, limits: ObservationLimits) -> Self {
        Self {
            cancelled,
            limits,
            started: Instant::now(),
            entries: 0,
            path_bytes: 0,
            artifact_bytes: 0,
        }
    }

    fn check(&self) -> Result<()> {
        ensure!(
            !self.cancelled.load(Ordering::Acquire),
            "Review observation cancelled"
        );
        ensure!(
            self.started.elapsed() <= self.limits.elapsed,
            "Review observation timed out"
        );
        Ok(())
    }
}

impl ReadBudget for ObservationBudget<'_> {
    type Error = anyhow::Error;

    fn entry(&mut self, depth: usize, bytes: usize) -> Result<()> {
        self.check()?;
        self.entries = self.entries.saturating_add(1);
        self.path_bytes = self.path_bytes.saturating_add(bytes);
        ensure!(
            depth <= 64
                && self.entries <= self.limits.entries
                && self.path_bytes <= self.limits.path_bytes,
            "Review observation entry budget exceeded"
        );
        Ok(())
    }

    fn artifact(&mut self, total: u64, chunk: usize) -> Result<()> {
        self.check()?;
        self.artifact_bytes = self.artifact_bytes.saturating_add(chunk as u64);
        ensure!(
            total <= self.limits.artifact_bytes
                && self.artifact_bytes <= self.limits.artifact_bytes,
            "Review observation artifact budget exceeded"
        );
        Ok(())
    }
}

fn source_failure(error: SourceFailure<anyhow::Error>) -> anyhow::Error {
    match error {
        SourceFailure::Budget(error) => error,
        SourceFailure::Io(kind) => anyhow!("Review source observation failed: {kind:?}"),
        SourceFailure::UnsafePath => anyhow!("Review source path is unsafe"),
    }
}

fn artifact_missing(
    inventory: &mut Option<Inventory>,
    relative: &str,
    expected_sha256: &str,
    budget: &mut ObservationBudget<'_>,
) -> Result<bool> {
    if relative.is_empty()
        || relative.len() > 4096
        || Path::new(relative).is_absolute()
        || relative.contains(['\\', '\0'])
        || matches!(relative.as_bytes(), [drive, b':', ..] if drive.is_ascii_alphabetic())
        || relative
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
        || expected_sha256.len() != 64
        || !expected_sha256
            .bytes()
            .all(|value| value.is_ascii_hexdigit())
    {
        return Ok(true);
    }
    let Some(inventory) = inventory else {
        return Ok(true);
    };
    let Some(index) = inventory.matching_path(relative) else {
        return Ok(true);
    };
    match inventory.hash(index, budget) {
        Ok((bytes, digest)) => Ok(bytes == 0 || digest != expected_sha256),
        Err(SourceFailure::Budget(error)) => Err(error),
        Err(SourceFailure::Io(_) | SourceFailure::UnsafePath) => Ok(true),
    }
}

#[derive(Default)]
pub(super) struct SnapshotFilamentLookup {
    integration_id: String,
    inventory: SpoolmanStock,
}

impl FilamentLookup for SnapshotFilamentLookup {
    fn for_part(
        &self,
        color_id: Option<&str>,
        spool_id: Option<&str>,
    ) -> Result<Option<ResolvedFilament>> {
        let Some(id) = color_id.and_then(|value| {
            value
                .trim()
                .strip_prefix(&format!("spoolman:{}:filament:", self.integration_id))?
                .parse::<i64>()
                .ok()
                .filter(|id| *id > 0)
        }) else {
            return Ok(None);
        };
        let Some(filament) = self.inventory.filaments.get(&id) else {
            return Ok(None);
        };
        let selected = spool_id.and_then(|value| {
            value
                .trim()
                .strip_prefix(&format!("spoolman:{}:spool:", self.integration_id))?
                .parse::<i64>()
                .ok()
                .filter(|id| *id > 0)
        });
        let spools = self
            .inventory
            .spools
            .iter()
            .filter(|spool| {
                spool.filament_id == id && selected.is_none_or(|selected| spool.id == selected)
            })
            .filter_map(|spool| {
                Some(SpoolSummary {
                    remaining_g: spool.remaining_g.filter(|grams| *grams > 0.0)?,
                    spool_id: spool.id,
                })
            })
            .collect();
        Ok(Some(ResolvedFilament {
            combo_label: filament.label.clone(),
            hex: Some(filament.hex.clone().unwrap_or_else(|| "#888888".into())),
            spools,
        }))
    }
}

struct FilamentDns(Arc<dyn pp_net::outbound::Resolver>);
impl reqwest::dns::Resolve for FilamentDns {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let future = self.0.resolve(name.as_str().into());
        Box::pin(async move {
            let addresses = future.await?;
            if addresses.is_empty()
                || addresses.len() > 32
                || !addresses.iter().all(|ip| pp_net::outbound::permitted(*ip))
            {
                return Err("Prohibited filament DNS answer".into());
            }
            Ok(Box::new(
                addresses
                    .into_iter()
                    .map(|ip| std::net::SocketAddr::new(ip, 0)),
            ) as reqwest::dns::Addrs)
        })
    }
}

async fn load_filament(
    config: &FilamentProviderConfig,
    cancelled: &AtomicBool,
) -> Result<SnapshotFilamentLookup> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .dns_resolver(Arc::new(FilamentDns(Arc::new(
            pp_net::outbound::SystemResolver,
        ))))
        .redirect(reqwest::redirect::Policy::none())
        .timeout(config.timeout)
        .build()?;
    let filaments = fetch_json(&client, config, "filament", cancelled).await?;
    let spools = fetch_json(&client, config, "spool", cancelled).await?;
    Ok(SnapshotFilamentLookup {
        integration_id: config.integration_id.clone(),
        inventory: parse_inventory(&filaments, &spools),
    })
}

async fn fetch_json(
    client: &reqwest::Client,
    config: &FilamentProviderConfig,
    resource: &str,
    cancelled: &AtomicBool,
) -> Result<Value> {
    tokio::time::timeout(config.timeout, async {
        let initial = config.endpoint(resource);
        let mut current = initial.clone();
        for redirects in 0..=config.max_redirects {
            ensure!(
                !cancelled.load(Ordering::Acquire),
                "Filament observation cancelled"
            );
            let mut request = client.get(current.clone());
            if let Some(api_key) = &config.api_key {
                request = request.bearer_auth(api_key.trim());
            }
            let response = request.send().await?;
            if matches!(
                response.status(),
                StatusCode::MOVED_PERMANENTLY
                    | StatusCode::FOUND
                    | StatusCode::SEE_OTHER
                    | StatusCode::TEMPORARY_REDIRECT
                    | StatusCode::PERMANENT_REDIRECT
            ) {
                ensure!(
                    redirects < config.max_redirects,
                    "Too many filament redirects"
                );
                let location = response
                    .headers()
                    .get(header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| anyhow!("Filament redirect has no location"))?;
                let next = current.join(location)?;
                ensure!(
                    next.origin() == initial.origin(),
                    "Filament redirect changed origin"
                );
                current = next;
                continue;
            }
            ensure!(
                response.status().is_success(),
                "Filament provider refused read"
            );
            let mut response = response;
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                ensure!(
                    !cancelled.load(Ordering::Acquire),
                    "Filament observation cancelled"
                );
                ensure!(
                    bytes.len().saturating_add(chunk.len()) <= config.max_response_bytes,
                    "Filament response too large"
                );
                bytes.extend_from_slice(&chunk);
            }
            return Ok(serde_json::from_slice(&bytes)?);
        }
        Err(anyhow!("Filament redirect limit exceeded"))
    })
    .await
    .map_err(|_| anyhow!("Filament observation timed out"))?
}

fn rows(value: &Value) -> &[Value] {
    list(value).unwrap_or(&[])
}

/// The rows of a Spoolman list response: a bare array, or an array under
/// `items`, `results` or `data`. `None` for any other shape.
pub(crate) fn list(value: &Value) -> Option<&[Value]> {
    if let Some(rows) = value.as_array() {
        return Some(rows);
    }
    value
        .as_object()
        .and_then(|object| {
            ["items", "results", "data"]
                .iter()
                .find_map(|key| object.get(*key)?.as_array())
        })
        .map(Vec::as_slice)
}

/// Spoolman's filament and spool lists, shared with the shortfall reader.
/// Archived spools are dropped; an unreadable remaining weight stays `None`.
pub(crate) fn parse_inventory(filaments: &Value, spools: &Value) -> SpoolmanStock {
    SpoolmanStock {
        filaments: parse_filaments(filaments),
        spools: parse_spools(spools),
    }
}

fn parse_filaments(value: &Value) -> HashMap<i64, InventoryFilament> {
    rows(value)
        .iter()
        .take(100_000)
        .filter_map(|row| {
            let object = row.as_object()?;
            let id = numeric(object.get("id")?)?;
            let name = text(object.get("name"), 1000).unwrap_or_else(|| format!("Filament {id}"));
            let material = text(object.get("material"), 1000).unwrap_or_default();
            let vendor = object
                .get("vendor")
                .and_then(|value| {
                    text(Some(value), 1000).or_else(|| {
                        value
                            .as_object()
                            .and_then(|value| text(value.get("name"), 1000))
                    })
                })
                .unwrap_or_default();
            let prefix = [vendor, material]
                .into_iter()
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            let label = if prefix.is_empty() {
                name
            } else {
                format!("{prefix} · {name}")
            };
            let hex = text(object.get("color_hex"), 20).and_then(|value| {
                let value = value.trim().trim_start_matches('#');
                (matches!(value.len(), 3 | 4 | 6 | 8)
                    && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
                .then(|| format!("#{}", value.to_ascii_lowercase()))
            });
            Some((id, InventoryFilament { label, hex }))
        })
        .collect()
}

fn parse_spools(value: &Value) -> Vec<InventorySpool> {
    rows(value)
        .iter()
        .take(100_000)
        .filter_map(|row| {
            let object = row.as_object()?;
            if object.get("archived").is_some_and(|value| {
                matches!(value, Value::Bool(true)) || value == 1 || value == "true"
            }) {
                return None;
            }
            let id = numeric(object.get("id")?)?;
            let filament_id = object.get("filament_id").and_then(numeric).or_else(|| {
                object
                    .get("filament")?
                    .as_object()?
                    .get("id")
                    .and_then(numeric)
            })?;
            let remaining_g = object
                .get("remaining_weight")
                .and_then(|value| value.as_f64().or_else(|| value.as_str()?.parse().ok()))
                .filter(|grams: &f64| grams.is_finite());
            Some(InventorySpool {
                id,
                filament_id,
                remaining_g,
            })
        })
        .collect()
}

fn numeric(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|value| *value > 0 && *value <= 9_007_199_254_740_991)
}

fn text(value: Option<&Value>, max: usize) -> Option<String> {
    let value = value?.as_str()?.trim();
    (!value.is_empty() && value.encode_utf16().count() <= max).then(|| value.into())
}

fn valid_hex(value: &str) -> bool {
    let value = value.trim().trim_start_matches('#');
    value.len() == 6 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
#[path = "review_observations_tests.rs"]
mod tests;
