//! Which stored schema versions a runtime opens, and the refusal for the rest.
//!
//! The refusal is terminal: it carries the version found and the supported
//! range, and a runtime that sees it stops instead of reopening the database.
//! Refusing never writes to the database; the version comes from the
//! preflight copy (`schema::preflight`).

/// Schema shipped by Print Partner v3.3.0, the oldest one upgraded in place.
pub const OLDEST_UPGRADABLE: u64 = 31;
/// The schema Node writes and the newest one the Node-backed runtime opens
/// (`currentSchemaVersion` in `web/apps/server/src/db/schema.ts`).
pub const NODE_SCHEMA: u64 = 34;
/// The newest schema this build's Rust writer opens.
pub const RUST_SCHEMA: u64 = crate::schema::CURRENT_VERSION;

/// The path a supported version takes on open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchemaPath {
    /// No database, an empty file, or no `schema_version` row: the full
    /// schema is installed.
    Fresh,
    /// 31 to 34, written by Node: Node's migrations replay first.
    NodeUpgrade(u64),
    /// From 35 up to the newest version: only the Rust steps after it run.
    RustUpgrade(u64),
    /// Already at the newest version: nothing to migrate.
    Current,
}

/// A stored schema outside the range a runtime opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IncompatibleSchema {
    pub found: u64,
    pub oldest: u64,
    pub newest: u64,
}

impl IncompatibleSchema {
    pub fn too_new(&self) -> bool {
        self.found > self.newest
    }

    /// One log line with numbers only: no path or database content.
    pub fn diagnostic(&self) -> String {
        format!(
            "incompatible_storage: found={} supported={}..={} direction={}",
            self.found,
            self.oldest,
            self.newest,
            if self.too_new() { "newer" } else { "older" }
        )
    }
}

impl std::fmt::Display for IncompatibleSchema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            found,
            oldest,
            newest,
        } = self;
        if self.too_new() {
            write!(
                f,
                "Incompatible storage: database schema version {found} is newer than supported version {newest} \
                 (this release opens schema {oldest} to {newest}). Install the Print Partner release that last \
                 opened this data folder, or restore a backup made by this release. The database was not changed."
            )
        } else {
            write!(
                f,
                "Incompatible storage: cannot upgrade database schema version {found} \
                 (this release opens schema {oldest} to {newest}). Install Print Partner v3.3.0, start it once \
                 so it upgrades the database, then install this release. The database was not changed."
            )
        }
    }
}

impl std::error::Error for IncompatibleSchema {}

/// The path `version` takes in a runtime that opens schemas up to `newest`.
pub fn classify(version: u64, newest: u64) -> Result<SchemaPath, IncompatibleSchema> {
    match version {
        0 => Ok(SchemaPath::Fresh),
        version if version < OLDEST_UPGRADABLE || version > newest => Err(IncompatibleSchema {
            found: version,
            oldest: OLDEST_UPGRADABLE,
            newest,
        }),
        version if version == newest => Ok(SchemaPath::Current),
        version if version <= NODE_SCHEMA => Ok(SchemaPath::NodeUpgrade(version)),
        version => Ok(SchemaPath::RustUpgrade(version)),
    }
}

thread_local! {
    /// The stored version the last `schema::preflight` on this thread read,
    /// or `None` when it stopped before reading one. Preflight runs on the
    /// caller's thread, so [`node_refusal`] reads it right after its call.
    static PREFLIGHT_VERSION: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

pub(crate) fn record_preflight_version(version: Option<u64>) {
    PREFLIGHT_VERSION.set(version);
}

/// The Node-backed runtime's schema check, run on its lease before the Node
/// child starts. `Ok(Some(_))` refuses the start. A Rust validation that fails
/// after the version was read still refuses a version Node cannot open. `Err`
/// is a failure before the version was read, such as a failed integrity
/// check; Node runs its own checks on that database, so the caller lets it
/// start.
pub fn node_refusal(
    lease: &crate::lease::StorageLease,
) -> anyhow::Result<Option<IncompatibleSchema>> {
    record_preflight_version(None);
    let version = match lease.preflight() {
        Ok(version) => version,
        Err(error) => match PREFLIGHT_VERSION.get() {
            Some(version) => version,
            None => return Err(error),
        },
    };
    Ok(classify(version, NODE_SCHEMA).err())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_supported_version_has_a_named_path() {
        assert_eq!(classify(0, RUST_SCHEMA), Ok(SchemaPath::Fresh));
        for version in 31..=34 {
            assert_eq!(
                classify(version, RUST_SCHEMA),
                Ok(SchemaPath::NodeUpgrade(version))
            );
        }
        for version in NODE_SCHEMA + 1..=RUST_SCHEMA {
            let path = if version == RUST_SCHEMA {
                SchemaPath::Current
            } else {
                SchemaPath::RustUpgrade(version)
            };
            assert_eq!(classify(version, RUST_SCHEMA), Ok(path));
        }
        assert_eq!(classify(34, NODE_SCHEMA), Ok(SchemaPath::Current));
    }

    #[test]
    fn versions_outside_the_range_are_refused_with_the_range() {
        for (found, newest) in [
            (1, RUST_SCHEMA),
            (30, RUST_SCHEMA),
            (RUST_SCHEMA + 1, RUST_SCHEMA),
            (35, NODE_SCHEMA),
            (u64::MAX, NODE_SCHEMA),
        ] {
            let refusal = classify(found, newest).unwrap_err();
            assert_eq!(
                refusal,
                IncompatibleSchema {
                    found,
                    oldest: 31,
                    newest
                }
            );
            assert_eq!(refusal.too_new(), found > newest);
            let text = refusal.to_string();
            assert!(text.starts_with("Incompatible storage: "), "{text}");
            assert!(text.contains(&format!("schema version {found} ")), "{text}");
            assert!(text.contains(&format!("schema 31 to {newest}")), "{text}");
            assert!(text.ends_with("The database was not changed."), "{text}");
        }
        assert_eq!(
            classify(30, 47).unwrap_err().diagnostic(),
            "incompatible_storage: found=30 supported=31..=47 direction=older"
        );
        assert_eq!(
            classify(48, 47).unwrap_err().diagnostic(),
            "incompatible_storage: found=48 supported=31..=47 direction=newer"
        );
    }
}
