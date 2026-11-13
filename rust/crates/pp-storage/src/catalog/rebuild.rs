use super::*;

#[derive(Debug)]
pub enum CatalogFailure {
    Input(String),
    DuplicateName(String),
    NotFound,
    InvalidStoredNaming,
    Referenced,
    TooLarge,
    Conflict,
}
impl std::fmt::Display for CatalogFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Input(detail) => detail,
            Self::DuplicateName(name) => return write!(f, "Source already exists: {name}"),
            Self::NotFound => "Source not found",
            Self::InvalidStoredNaming => "Stored Source naming settings are invalid",
            Self::Referenced => "Source historical name is referenced and cannot be changed",
            Self::TooLarge => "Catalog request too large",
            Self::Conflict => "Catalog metadata changed; retry from the current version",
        })
    }
}
impl std::error::Error for CatalogFailure {}
#[derive(Clone)]
pub struct CatalogAccess {
    client: SettingsClient,
    policy: auth::AuthPolicy,
}
#[derive(Clone)]
pub struct CatalogKeyAccess {
    access: CatalogAccess,
    tenant: String,
}
impl CatalogAccess {
    pub fn session(&self, secret: auth::Secret) -> SourceCatalogClient {
        SourceCatalogClient {
            client: self.client.clone(),
            authority: Authority::Credentials(Credentials::Session(secret), self.policy),
        }
    }
}
impl CatalogKeyAccess {
    pub fn key(&self, secret: auth::Secret) -> SourceCatalogClient {
        SourceCatalogClient {
            client: self.access.client.clone(),
            authority: Authority::Credentials(
                Credentials::Key {
                    tenant_id: self.tenant.clone(),
                    key: secret,
                },
                self.access.policy,
            ),
        }
    }
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NumericCategoryCommand {
    selectors: Vec<serde_json::Number>,
    category: Option<String>,
}
impl NumericCategoryCommand {
    pub fn from_finite_numbers(
        selectors: impl IntoIterator<Item = f64>,
        category: Option<String>,
    ) -> std::result::Result<Self, CatalogFailure> {
        Ok(Self {
            selectors: selectors
                .into_iter()
                .map(|value| {
                    if !value.is_finite() {
                        return Err(CatalogFailure::Input("Invalid Source lookup".into()));
                    }
                    if value.fract() == 0.0 && value.abs() <= 9_007_199_254_740_991.0 {
                        Ok(serde_json::Number::from(value as i64))
                    } else {
                        serde_json::Number::from_f64(value)
                            .ok_or_else(|| CatalogFailure::Input("Invalid Source lookup".into()))
                    }
                })
                .collect::<std::result::Result<_, _>>()?,
            category,
        })
    }
}
#[derive(Debug, Serialize)]
pub struct BulkCategoryResult {
    succeeded: usize,
    failed: usize,
    updated: Vec<SourceSummary>,
    results: Vec<BulkCategoryItem>,
}
#[derive(Debug, Serialize)]
struct BulkCategoryItem {
    source_id: serde_json::Number,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct CategorySettings {
    pub categories: Vec<String>,
    pub tree: Vec<CategoryNode>,
}
#[derive(Debug, Serialize)]
pub struct CategoryNode {
    pub path: String,
    pub name: String,
    pub depth: usize,
    pub parent: Option<String>,
    pub children: Vec<CategoryNode>,
}
impl CategorySettings {
    pub(super) fn new(categories: Vec<String>) -> Self {
        let tree = nodes(&categories, None);
        Self { categories, tree }
    }
}
pub const IMPORT_RULE_BODY_LIMIT: usize = 8 * 1024 * 1024;
fn apply_category(
    tx: &Transaction<'_>,
    tenant: &str,
    selected: Vec<(Option<i64>, serde_json::Number)>,
    category: Option<String>,
) -> Result<BulkCategoryResult> {
    let mut results = Vec::new();
    let mut updated = Vec::new();
    for (id, source_id) in selected {
        if let Some(id) = id
            && get(tx, tenant, id)?.is_some()
        {
            updated.push(patch(
                tx,
                tenant,
                id,
                SourcePatch {
                    metadata: Some(Map::from_iter([(
                        "category".into(),
                        json!(categories::normalize(category.as_deref().unwrap_or(""))),
                    )])),
                    ..Default::default()
                },
            )?);
            results.push(BulkCategoryItem {
                source_id,
                ok: true,
                detail: None,
            });
        } else {
            results.push(BulkCategoryItem {
                source_id,
                ok: false,
                detail: Some("Source not found".into()),
            });
        }
    }
    let succeeded = updated.len();
    Ok(BulkCategoryResult {
        succeeded,
        failed: results.len() - succeeded,
        updated,
        results,
    })
}

pub(super) fn request_size(request: &Request) -> Result<()> {
    let (length, limit) = match request {
        Request::SaveImportRules { rules, .. } => (
            serde_json::to_vec(&json!({"rules": rules}))?.len(),
            IMPORT_RULE_BODY_LIMIT,
        ),
        _ => (serde_json::to_vec(request)?.len(), 1024 * 1024),
    };
    ensure!(length <= limit, CatalogFailure::TooLarge);
    Ok(())
}

pub(super) fn create(
    tx: &Transaction<'_>,
    tenant: &str,
    mut source: CreateSource,
) -> Result<CreateSource> {
    location::create(&mut source)?;
    let name = trim(&source.name);
    ensure!(
        !name.is_empty(),
        CatalogFailure::Input("Source name is required".into())
    );
    let duplicate: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE tenant_id=?1 AND name=?2)",
        params![tenant, name],
        |r| r.get(0),
    )?;
    ensure!(!duplicate, CatalogFailure::DuplicateName(name.into()));
    Ok(source)
}
/// `POST /sources`'s create, inside a write transaction another writer
/// already holds: the planning brief's git URL inputs create their Source in
/// the same transaction as the brief. `repos` is the data folder's `repos`.
pub(crate) fn create_in(
    tx: &Transaction<'_>,
    repos: &std::path::Path,
    tenant: &str,
    source: CreateSource,
) -> Result<SourceSummary> {
    let source = create(tx, tenant, source)?;
    super::create_source(tx, repos, tenant, source)
}
pub(super) fn update(
    tx: &Transaction<'_>,
    tenant: &str,
    id: i64,
    mut patch: SourcePatch,
) -> Result<SourcePatch> {
    let existing = get(tx, tenant, id)?.ok_or(CatalogFailure::NotFound)?;
    if super::metadata_guard_enabled()
        && let Some(expected) = patch.metadata_version
    {
        let current = existing.catalog_metadata_version.unwrap_or(0);
        ensure!(expected == current, CatalogFailure::Conflict);
    }
    if let Some(name) = &patch.name {
        let name = trim(name);
        ensure!(
            !name.is_empty(),
            CatalogFailure::Input("Source name is required".into())
        );
        ensure!(
            name == existing.name || !name_referenced(tx, tenant, &existing.name)?,
            CatalogFailure::Referenced
        );
        let duplicate: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE tenant_id=?1 AND name=?2 AND id<>?3)",
            params![tenant, name, id],
            |r| r.get(0),
        )?;
        ensure!(!duplicate, CatalogFailure::DuplicateName(name.into()));
    }
    location::update(&existing, &mut patch)?;
    Ok(patch)
}
pub(super) fn apply_numeric_category(
    tx: &Transaction<'_>,
    tenant: &str,
    command: NumericCategoryCommand,
) -> Result<BulkCategoryResult> {
    ensure!(
        !command.selectors.is_empty(),
        CatalogFailure::Input("source_ids must be a non-empty array".into())
    );
    let mut seen = Vec::new();
    let mut selected = Vec::new();
    for number in command.selectors {
        if seen.contains(&number) {
            continue;
        }
        let id = number.as_i64();
        seen.push(number.clone());
        selected.push((id, number));
    }
    apply_category(tx, tenant, selected, command.category)
}

fn nodes(paths: &[String], parent: Option<&str>) -> Vec<CategoryNode> {
    paths
        .iter()
        .filter(|p| {
            p.rsplit_once('/').map(|(p, _)| p.to_lowercase()) == parent.map(str::to_lowercase)
        })
        .map(|p| CategoryNode {
            path: p.clone(),
            name: p.rsplit('/').next().unwrap_or(p).into(),
            depth: p.matches('/').count(),
            parent: p.rsplit_once('/').map(|(p, _)| p.into()),
            children: nodes(paths, Some(p)),
        })
        .collect()
}

impl WriterOwner {
    pub fn catalog_access(&self, policy: auth::AuthPolicy) -> Result<CatalogAccess> {
        auth::validate_policy(policy)?;
        Ok(CatalogAccess {
            client: self.client(),
            policy,
        })
    }
    pub fn catalog_key_access(
        &self,
        policy: auth::AuthPolicy,
        tenant: String,
    ) -> Result<CatalogKeyAccess> {
        ensure!(
            !tenant.is_empty() && tenant.len() <= 512,
            CatalogFailure::Input("Invalid routed tenant".into())
        );
        Ok(CatalogKeyAccess {
            access: self.catalog_access(policy)?,
            tenant,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::value::{Error as ValueError, F64Deserializer};

    #[test]
    fn numeric_selectors_reject_nonfinite_deserialization() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(
                serde_json::Number::deserialize(F64Deserializer::<ValueError>::new(value)).is_err()
            );
        }
    }
}
