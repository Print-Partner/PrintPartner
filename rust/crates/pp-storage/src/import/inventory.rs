//! The conversion inventory: every stored Node shape the import converts,
//! checks or copies. `docs/node-import.md` holds the same table with each
//! shape's Rust reader; a test fails when a shape here is not named there.

use Shape::{Column, File, Setting, SettingPrefix, SourceFile, Table};
use std::fmt;

/// Where one stored shape lives in a Node installation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// The `app_settings` row with this key. Its items are the elements of
    /// the stored array, or one for any other value.
    Setting(&'static str),
    /// Every `app_settings` row whose key starts with this prefix, one item
    /// per row.
    SettingPrefix(&'static str),
    /// A column. Its items are the rows that hold a value.
    Column(&'static str, &'static str),
    /// A whole table, one item per row.
    Table(&'static str),
    /// A JSON file directly in the data folder, counted like a setting.
    File(&'static str),
    /// Every file of this name under `repos/`.
    SourceFile(&'static str),
}

impl fmt::Display for Shape {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Setting(key) => write!(f, "setting {key}"),
            Self::SettingPrefix(prefix) => write!(f, "setting {prefix}*"),
            Self::Column(table, column) => write!(f, "{table}.{column}"),
            Self::Table(table) => write!(f, "{table}.*"),
            Self::File(name) => write!(f, "file {name}"),
            Self::SourceFile(name) => write!(f, "file repos/**/{name}"),
        }
    }
}

/// Whether the import handles a row yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// No converter or check yet.
    Pending,
    /// The import converts the row.
    Converted,
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending => f.write_str("pending"),
            Self::Converted => f.write_str("converted"),
        }
    }
}

/// One inventory row: the shapes one converter or check handles.
#[derive(Debug, PartialEq, Eq)]
pub struct Row {
    /// The import step, 2 to 6, that handles the row. Step 1 is the dry run.
    pub step: u8,
    pub shapes: &'static [Shape],
    /// The typed Rust value the row becomes.
    pub target: &'static str,
    pub status: Status,
}

const fn pending(step: u8, shapes: &'static [Shape], target: &'static str) -> Row {
    Row {
        step,
        shapes,
        target,
        status: Status::Pending,
    }
}

/// The rows the import converts or checks, in step order.
#[rustfmt::skip]
pub const ROWS: [Row; 16] = [
    Row { status: Status::Converted, ..pending(2, &[Column("projects", "local_path")], "<data>/repos/<snapshot_locator>") },
    pending(3, &[Setting("printer.fleet")], "typed fleet document"),
    pending(3, &[Setting("integrations")], "typed integrations document"),
    pending(4, &[Setting("printer.checkoff_links")], "Vec<CheckoffLink>"),
    pending(4, &[Setting("printer.send_queue")], "printer_send_queue rows"),
    pending(4, &[Setting("printer.unattributed_prints"), Setting("printer.plan_bindings")], "Vec<UnattributedPrint>, Vec<PlanBinding>"),
    pending(5, &[Column("projects", "metadata_json")], "SourceMetadata"),
    pending(5, &[Column("projects", "imported_paths")], "Vec<String>"),
    pending(5, &[Setting("source_categories"), Column("app_events", "payload_json")], "Vec<String>, SourceEventPayload"),
    // ast-grep-ignore: no-writer-family-literal -- Names the Node file the import reads; this module writes nothing.
    pending(5, &[File("custom_filaments.json")], "Vec<CustomFilament>"),
    pending(6, &[Setting("feature_toggles"), SettingPrefix("feature_toggles:plan:")], "pp_domain::features::Toggles"),
    pending(6, &[SettingPrefix("production_setup:")], "ProductionSetup"),
    pending(6, &[SettingPrefix("role_filaments_"), SettingPrefix("plan_variant_selection_"), SettingPrefix("kit_manifest_")], "one struct per setting"),
    pending(6, &[Column("plan_snapshots", "payload_json")], "SnapshotPayload"),
    pending(6, &[Setting("api_keys_v1")], "StoredKey"),
    pending(6, &[
        Setting("stl_naming_defaults"),
        SettingPrefix("build_planning.v1."),
        // The key ends in free text; the report counts these keys and never prints them.
        SettingPrefix("build_planning.create."),
        Column("plan_decisions", "params_json"),
        Column("plan_decisions", "result_json"),
        Column("plan_draft_required_unit_reconciliations", "selection_basis_json"),
        Column("plan_draft_required_unit_reconciliations", "result_json"),
        SourceFile(".printpartner-source-snapshot.json"),
    ], "checked with today's typed reader, not rewritten"),
];

/// What the import copies byte for byte and no converter may rewrite: slicer
/// profile text, which Rust reads and keeps opaque (`profiles.rs`), and the
/// two credentials Rust reads as plain settings (`app_settings.rs`).
pub const COPIED: [Shape; 8] = [
    Column("printer_profiles", "raw_json"),
    Column("printer_profiles", "resolved_flat_config"),
    Column("process_profiles", "resolved_flat_config"),
    Column("filament_profiles", "raw_json"),
    Column("filament_profiles", "resolved_flat_config"),
    Column("filament_profiles", "retraction"),
    // Credentials. They stay plain settings on every platform, with no
    // converter and no keychain. The report prints the key, never the value.
    Setting("github_pat"),
    Setting("discord_notify_webhook_url"),
];

/// Stored data no Rust code reads. The import keeps it byte for byte, also
/// for features the product has cut.
pub const UNUSED: [Shape; 11] = [
    Table("board_posts"),
    Table("plan_shares"),
    Table("printer_telemetry"),
    Table("slicer_instances"),
    SettingPrefix("filename_grouping:"),
    SettingPrefix("manifest_v2_"),
    Setting("integration_webhooks_v1"),
    Setting("printer.print_outcomes"),
    Setting("assistant_feedback"),
    Setting("assistant_chat_history"),
    Setting("assistant_daily_usage"),
];

impl Shape {
    /// Whether this shape covers the setting `key`.
    pub fn names_setting(&self, key: &str) -> bool {
        match self {
            Self::Setting(name) => key == *name,
            Self::SettingPrefix(prefix) => key.starts_with(prefix),
            _ => false,
        }
    }

    /// Whether this shape covers `column` of `table`.
    pub fn names_column(&self, table: &str, column: &str) -> bool {
        match self {
            Self::Column(t, c) => table == *t && column == *c,
            Self::Table(t) => table == *t,
            _ => false,
        }
    }
}

/// Every shape the inventory names: converted, copied or kept unused.
pub fn named() -> impl Iterator<Item = &'static Shape> {
    let rows = ROWS.iter().flat_map(|row| row.shapes);
    rows.chain(&COPIED).chain(&UNUSED)
}
