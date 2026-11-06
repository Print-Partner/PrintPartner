//! Managed accepted Plates (GRE-419), as Node's `routes/accepted-plates.ts`
//! and `db/accepted-plates.ts` serve them. This module reads the Plate
//! workspace: the Accepted Plan's Required units, the head Plate revision's
//! layout checked against its digest, and the fleet Printers a Plate can use.
//!
//! Compiled with `desktop_rebuild`; service callers must check `PP_DESKTOP_REBUILD`.

use crate::{
    Envelope, Shared, WriterOwner, auth,
    production::{
        layout::{
            Placement, PlateInput, PrinterGeometry, Spacing, UnitInput, ValidatedPlate,
            layout_digest, validate_plates,
        },
        model::{
            Digest, DimensionUm, MAX_JS_SAFE_INTEGER, MAX_PLATE_UM, OffsetUm, PlateId,
            RequiredUnitToken, millimetres_to_micrometres, trim_ecmascript,
        },
    },
    read_model::{
        AcceptedRead, Credential,
        farm::setting_list,
        graph::{self, Snapshot},
        read_tenant, submit_read,
        views::catalog_color,
    },
};
use anyhow::{Result, anyhow, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::{Map, Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

const LAYOUT_FORMAT: u8 = 2;
const LEGACY_LAYOUT_FORMAT: u8 = 1;

/// HTTP status and JSON body, matching the Node route. A failure body is
/// `{ detail, code, ...fields }`.
#[derive(Clone, Debug, PartialEq)]
pub struct Response {
    pub status: u16,
    pub body: Value,
}

/// One request with its raw path segments; storage validates them as the
/// Node route does.
pub enum Request {
    Read { profile: String },
}

#[derive(Clone)]
pub struct PlatesClient {
    shared: Arc<Shared>,
    repos: PathBuf,
    policy: auth::AuthPolicy,
}

pub(crate) struct Command {
    credential: Credential,
    op: Op,
    repos: PathBuf,
    policy: auth::AuthPolicy,
}

impl WriterOwner {
    pub fn accepted_plates(&self, policy: auth::AuthPolicy) -> Result<PlatesClient> {
        auth::validate_policy(policy)?;
        Ok(PlatesClient {
            shared: self.client.shared.clone(),
            repos: self.repos_dir()?,
            policy,
        })
    }
}

impl PlatesClient {
    /// Returns the Node response. `Err` means authentication failed or the
    /// outcome is unknown after admission; reread the Plates before retrying.
    pub fn apply(
        &self,
        credential: Credential,
        request: Request,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Response> {
        let op = match validate(request) {
            Ok(op) => op,
            Err(response) => return Ok(response),
        };
        submit_read(&self.shared, cancelled, wait, |reply| Envelope::Plates {
            command: Command {
                credential,
                op,
                repos: self.repos.clone(),
                policy: self.policy,
            },
            reply,
        })
    }
}

/// The Accepted Plan a caller observed, as Node's `parseBasis` reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Basis {
    profile_id: i64,
    plan_version: i64,
    revision_id: i64,
    revision_digest: String,
    mapping_digest: String,
}
impl Basis {
    fn of(snapshot: &Snapshot) -> Self {
        Self {
            profile_id: snapshot.profile.id,
            plan_version: snapshot.plan_version,
            revision_id: snapshot.revision_id,
            revision_digest: snapshot.revision_digest.clone(),
            mapping_digest: snapshot.required_unit_mapping_digest.clone(),
        }
    }
    fn json(&self) -> Value {
        json!({
            "profile_id": self.profile_id,
            "plan_version": self.plan_version,
            "plan_revision_id": self.revision_id,
            "plan_revision_digest": self.revision_digest,
            "required_unit_mapping_digest": self.mapping_digest,
        })
    }
    fn stamps(&self, revision: &Revision) -> bool {
        revision.plan_revision_id == self.revision_id
            && revision.plan_version == self.plan_version
            && revision.plan_revision_digest == self.revision_digest
            && revision.mapping_digest == self.mapping_digest
    }
}

/// Node's `positiveSafeInteger`; a non-finite number is not one.
fn positive(number: f64) -> Option<i64> {
    (number.fract() == 0.0 && (1.0..=MAX_JS_SAFE_INTEGER as f64).contains(&number))
        .then_some(number as i64)
}

enum Op {
    Read(i64),
}

fn invalid() -> Response {
    failure(400, "invalid_request", "Request is invalid", Map::new())
}

fn failure(status: u16, code: &str, detail: &str, mut fields: Map<String, Value>) -> Response {
    let mut body = Map::new();
    body.insert("detail".into(), detail.into());
    body.insert("code".into(), code.into());
    body.append(&mut fields);
    Response {
        status,
        body: Value::Object(body),
    }
}

/// Node's route check: a decimal Plan id.
fn validate(request: Request) -> std::result::Result<Op, Response> {
    match request {
        Request::Read { profile } => profile_id(&profile).map(Op::Read).ok_or_else(invalid),
    }
}

/// Node's `profileId`: `/^\d+$/`, then a positive safe integer.
fn profile_id(id: &str) -> Option<i64> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    positive(id.parse().ok()?)
}

/// A read that returns no workspace, as Node's `sendReadFailure` answers it.
enum Refusal {
    StateUnavailable,
    ProfileNotFound,
}
impl Refusal {
    fn response(self) -> Response {
        let (status, code, detail) = match self {
            Self::StateUnavailable => (
                409,
                "accepted_state_unavailable",
                "Accepted Plan state is unavailable",
            ),
            Self::ProfileNotFound => (404, "profile_not_found", "Plan not found"),
        };
        failure(status, code, detail, Map::new())
    }
}

/// Stored Plate data that contradicts its own revision. Answered as Node's
/// `AcceptedPlateIntegrityError`: 500 with no detail.
#[derive(Debug)]
struct Integrity(&'static str);
impl std::fmt::Display for Integrity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Accepted Plate integrity check failed: {}", self.0)
    }
}
impl std::error::Error for Integrity {}

fn unavailable() -> Response {
    failure(
        503,
        "accepted_plate_update_unavailable",
        "Accepted Plate update is unavailable",
        Map::new(),
    )
}

fn inconsistent() -> Response {
    failure(
        500,
        "internal_error",
        "Accepted Plate data is inconsistent",
        Map::new(),
    )
}

pub(crate) fn execute(connection: &mut Connection, command: Command) -> Result<Response> {
    let Command {
        credential,
        op: Op::Read(profile),
        repos,
        policy,
    } = command;
    let Ok(tx) = connection.transaction_with_behavior(TransactionBehavior::Deferred) else {
        return Ok(unavailable());
    };
    let tenant = read_tenant(&tx, &credential, policy)?;
    let response = match read(&tx, &tenant, profile, &repos) {
        Ok(response) => response,
        Err(error) if error.is::<Integrity>() || error.is::<graph::Integrity>() => inconsistent(),
        Err(error) => return Err(error),
    };
    // Authentication may have updated key metadata, so only success commits.
    if response.status == 200 && tx.commit().is_err() {
        return Ok(inconsistent());
    }
    Ok(response)
}

/// One `accepted_plate_revisions` row.
struct Revision {
    id: i64,
    plan_revision_id: i64,
    plan_version: i64,
    plan_revision_digest: String,
    mapping_digest: String,
    layout_digest: String,
    plate_count: i64,
    unit_count: i64,
    number: i64,
    undo_from: Option<i64>,
}

fn revision(tx: &Transaction<'_>, tenant: &str, profile: i64, id: i64) -> Result<Option<Revision>> {
    Ok(tx
        .query_row(
            "SELECT id,plan_revision_id,plan_version,plan_revision_digest,required_unit_mapping_digest,layout_digest,expected_plate_count,expected_unit_count,revision_number,undo_from_revision_id FROM accepted_plate_revisions WHERE tenant_id=?1 AND profile_id=?2 AND id=?3",
            params![tenant, profile, id],
            |row| {
                Ok(Revision {
                    id: row.get(0)?,
                    plan_revision_id: row.get(1)?,
                    plan_version: row.get(2)?,
                    plan_revision_digest: row.get(3)?,
                    mapping_digest: row.get(4)?,
                    layout_digest: row.get(5)?,
                    plate_count: row.get(6)?,
                    unit_count: row.get(7)?,
                    number: row.get(8)?,
                    undo_from: row.get(9)?,
                })
            },
        )
        .optional()?)
}

fn head(tx: &Transaction<'_>, tenant: &str, profile: i64) -> Result<Option<i64>> {
    Ok(tx
        .query_row(
            "SELECT current_revision_id FROM accepted_plate_heads WHERE tenant_id=?1 AND profile_id=?2",
            params![tenant, profile],
            |row| row.get(0),
        )
        .optional()?)
}

fn visible(tx: &Transaction<'_>, tenant: &str, profile: i64) -> Result<bool> {
    Ok(tx
        .query_row(
            "SELECT 1 FROM build_profiles WHERE tenant_id=?1 AND id=?2",
            params![tenant, profile],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// The Required units of included Parts in snapshot order: what a Plate may
/// hold.
fn required(snapshot: &Snapshot) -> Vec<(&graph::Part, &graph::Unit)> {
    snapshot
        .parts
        .iter()
        .filter(|part| part.included)
        .flat_map(|part| {
            part.units
                .iter()
                .filter(|unit| unit.required)
                .map(move |unit| (part, unit))
        })
        .collect()
}

fn tokens(snapshot: &Snapshot) -> Result<HashSet<RequiredUnitToken>> {
    required(snapshot)
        .into_iter()
        .map(|(_, unit)| {
            RequiredUnitToken::parse(unit.token.as_str()).map_err(|_| anyhow!(Integrity("layout")))
        })
        .collect()
}

/// Node's `readAcceptedPlateRevisionLayoutInternal`: the revision's Plates,
/// checked against its counts, tenant and layout digest, either format.
fn layout(
    tx: &Transaction<'_>,
    tenant: &str,
    revision: &Revision,
    expected: &HashSet<RequiredUnitToken>,
) -> Result<Vec<ValidatedPlate>> {
    let corrupt = |code| anyhow!(Integrity(code));
    let size = |value: i64| DimensionUm::parse(value).ok();
    let offset = |value: i64| OffsetUm::parse(value).ok();
    let units = tx
        .prepare("SELECT tenant_id,plate_id,required_unit_token,x_um,y_um,width_um,depth_um,height_um,placement,pinned FROM accepted_plate_units WHERE revision_id=?1 ORDER BY required_unit_token")?
        .query_map([revision.id], |row| {
            let unit = (|| -> rusqlite::Result<Option<UnitInput>> {
                let placement = match row.get::<_, String>(8)?.as_str() {
                    "manual" => Placement::Manual,
                    "unplaced" => Placement::Unplaced,
                    _ => Placement::Auto,
                };
                Ok((|| {
                    Some(UnitInput {
                        token: RequiredUnitToken::parse(row.get::<_, String>(2).ok()?).ok()?,
                        x_um: offset(row.get(3).ok()?)?,
                        y_um: offset(row.get(4).ok()?)?,
                        width_um: size(row.get(5).ok()?)?,
                        depth_um: size(row.get(6).ok()?)?,
                        height_um: size(row.get(7).ok()?)?,
                        placement,
                        pinned: row.get::<_, i64>(9).ok()? == 1,
                    })
                })())
            })()?;
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, unit))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let plates = tx
        .prepare("SELECT tenant_id,plate_id,ordinal,printer_id,printer_name,printer_model,bed_width_um,bed_depth_um,bed_height_um,margin_um FROM accepted_plates WHERE revision_id=?1 ORDER BY ordinal")?
        .query_map([revision.id], |row| {
            let plate_id: String = row.get(1)?;
            let plate = (|| {
                Some(PlateInput {
                    plate_id: PlateId::stored(&plate_id)?,
                    printer_id: row.get(3).ok()?,
                    printer_name: row.get(4).ok()?,
                    printer_model: row.get(5).ok()?,
                    printer: PrinterGeometry {
                        bed_width_um: size(row.get(6).ok()?)?,
                        bed_depth_um: size(row.get(7).ok()?)?,
                        bed_height_um: size(row.get(8).ok()?)?,
                        margin_um: offset(row.get(9).ok()?)?,
                    },
                    units: Vec::new(),
                })
            })();
            Ok((row.get::<_, String>(0)?, plate_id, row.get::<_, i64>(2)?, plate))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        plates.len() as i64 == revision.plate_count && units.len() as i64 == revision.unit_count,
        corrupt("counts")
    );
    let ids: HashSet<&str> = plates.iter().map(|plate| plate.1.as_str()).collect();
    ensure!(
        plates.iter().all(|plate| plate.0 == tenant)
            && units
                .iter()
                .all(|unit| unit.0 == tenant && ids.contains(unit.1.as_str()))
            && (1..)
                .zip(&plates)
                .all(|(ordinal, plate)| plate.2 == ordinal),
        corrupt("layout")
    );
    let mut by_plate: HashMap<&str, Vec<_>> = HashMap::new();
    for (_, owner, unit) in &units {
        by_plate.entry(owner.as_str()).or_default().push(unit);
    }
    let mut stored = Vec::with_capacity(plates.len());
    for (_, id, _, plate) in plates {
        let mut plate = plate.ok_or_else(|| corrupt("layout"))?;
        for unit in by_plate.remove(id.as_str()).unwrap_or_default() {
            plate
                .units
                .push(unit.clone().ok_or_else(|| corrupt("layout"))?);
        }
        stored.push(plate);
    }
    let legacy = validate_plates(stored.clone(), expected, false, Spacing::OverlapOnly)
        .map_err(|_| corrupt("layout"))?;
    if layout_digest(&legacy, LEGACY_LAYOUT_FORMAT).as_str() == revision.layout_digest {
        return Ok(legacy);
    }
    let current = validate_plates(stored, expected, false, Spacing::Clearance)
        .map_err(|_| corrupt("layout"))?;
    ensure!(
        layout_digest(&current, LAYOUT_FORMAT).as_str() == revision.layout_digest,
        corrupt("layout_digest")
    );
    Ok(current)
}

/// The tenant's Ready Accepted Plan, or why it cannot be read.
fn accepted(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    repos: &Path,
) -> Result<std::result::Result<Snapshot, AcceptedRead>> {
    Ok(
        match graph::read(tx, tenant, profile, repos, &mut graph::Budget::default())? {
            AcceptedRead::Ready { snapshot } => Ok(*snapshot),
            other => Err(other),
        },
    )
}

fn placement(value: Placement) -> &'static str {
    match value {
        Placement::Auto => "auto",
        Placement::Manual => "manual",
        Placement::Unplaced => "unplaced",
    }
}

/// A fleet Printer with usable build geometry: Node's `acceptedPrinter` over
/// `parsePrinterMachine`.
#[derive(Clone)]
struct Printer {
    id: String,
    name: String,
    model: String,
    um: [i64; 4],
}
impl Printer {
    fn json(&self) -> Value {
        let [width, depth, height, margin] = self.um;
        json!({
            "id": self.id,
            "name": self.name,
            "model": self.model,
            "bed_width_um": width,
            "bed_depth_um": depth,
            "bed_height_um": height,
            "margin_um": margin,
        })
    }
}

/// JavaScript `String(value)` for a parsed JSON value.
fn js_string(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => match number.as_f64() {
            Some(float) if float.fract() == 0.0 && float.abs() < 1e21 => format!("{float:.0}"),
            _ => number.to_string(),
        },
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::Null => String::new(),
                other => js_string(other),
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

/// JavaScript `Number(value ?? fallback)` for a parsed JSON value; `None`
/// stands for NaN. An array converts through its `toString`, as in
/// JavaScript.
fn js_number(value: &Value, fallback: f64) -> Option<f64> {
    match value {
        Value::Null => Some(fallback),
        Value::Bool(flag) => Some(f64::from(u8::from(*flag))),
        Value::Number(number) => number.as_f64(),
        Value::String(text) => {
            let text = trim_ecmascript(text);
            if text.is_empty() {
                return Some(0.0);
            }
            let radix = |digits: &str, radix: u32| {
                (!digits.is_empty() && digits.chars().all(|c| c.is_digit(radix)))
                    .then(|| u128::from_str_radix(digits, radix).ok())
                    .flatten()
                    .map(|value| value as f64)
            };
            match text.get(..2) {
                Some("0x" | "0X") => return radix(&text[2..], 16),
                Some("0o" | "0O") => return radix(&text[2..], 8),
                Some("0b" | "0B") => return radix(&text[2..], 2),
                _ => {}
            }
            let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
            if digits
                .chars()
                .any(|c| c.is_ascii_alphabetic() && c != 'e' && c != 'E')
            {
                return None;
            }
            text.parse().ok()
        }
        Value::Array(_) => js_number(&Value::String(js_string(value)), fallback),
        Value::Object(_) => None,
    }
}

/// One `printer.fleet` row as `acceptedPrinter(parsePrinterMachine(row))`.
fn accepted_printer(row: &Value) -> Option<Printer> {
    let um = |key: &str, fallback: f64| {
        let value = js_number(row.get(key).unwrap_or(&Value::Null), fallback)?;
        millimetres_to_micrometres(value)
    };
    let text = |value: &Value| trim_ecmascript(&js_string(value)).to_owned();
    let name = row
        .get("name")
        .filter(|v| !v.is_null())
        .unwrap_or(&json!("Printer"))
        .clone();
    let model = row
        .get("model")
        .filter(|v| !v.is_null())
        .unwrap_or(&name)
        .clone();
    let id = text(row.get("id").unwrap_or(&json!("undefined")));
    let (name, model) = (text(&name), text(&model));
    let width = um("bed_width_mm", 250.0)?;
    let depth = um("bed_depth_mm", 210.0)?;
    let height = match row.get("bed_height_mm") {
        None | Some(Value::Null) => return None,
        Some(value) => millimetres_to_micrometres(js_number(value, 0.0)?)?,
    };
    let margin = um("margin_mm", 4.0)?;
    if id.is_empty()
        || name.is_empty()
        || model.is_empty()
        || width <= 0
        || depth <= 0
        || height <= 0
        || margin < 0
        || margin * 2 >= width
        || margin * 2 >= depth
    {
        return None;
    }
    Some(Printer {
        id,
        name,
        model,
        um: [width, depth, height, margin],
    })
}

/// Node's `loadFleet` mapped through `acceptedPrinter`, by raw id. A `null`
/// row makes Node's parse throw, which empties the whole fleet.
fn fleet(tx: &Transaction<'_>, tenant: &str) -> Result<Vec<(String, Option<Printer>)>> {
    let rows = setting_list(tx, tenant, "printer.fleet")?;
    if rows.iter().any(Value::is_null) {
        return Ok(Vec::new());
    }
    Ok(rows
        .iter()
        .map(|row| {
            let raw = js_string(row.get("id").unwrap_or(&json!("undefined")));
            (
                raw,
                row.is_object().then(|| accepted_printer(row)).flatten(),
            )
        })
        .collect())
}

/// Node's `setupUnit` over `acceptedPlateSetupUnits`.
fn setup_unit(part: &graph::Part, unit: &graph::Unit) -> Map<String, Value> {
    let mut row = Map::new();
    let directory = part
        .relative_path
        .rfind('/')
        .map_or("", |index| &part.relative_path[..index]);
    for (key, value) in [
        ("token", json!(unit.token)),
        ("part_id", json!(part.projection_part_id)),
        ("object_name", json!(unit.object_name)),
        ("filename", json!(part.filename)),
        ("relative_path", json!(part.relative_path)),
        ("source_directory", json!(directory)),
        ("source_layer", json!(part.source_layer)),
        ("role", json!(part.effective_role)),
        ("filament_color_id", json!(part.filament_color_id)),
    ] {
        row.insert(key.into(), value);
    }
    if let Some(custom) = &part.filament_custom_hex {
        row.insert("filament_custom_hex".into(), json!(custom));
    }
    let hex = match (&part.filament_custom_hex, &part.filament_color_id) {
        (Some(custom), _) if !custom.is_empty() => Some(custom.clone()),
        (_, Some(id)) if !id.is_empty() => catalog_color(id).map(|color| color.hex.clone()),
        _ => None,
    };
    if let Some(hex) = hex {
        row.insert("filament_hex".into(), json!(hex));
    }
    row.insert("completed".into(), json!(unit.completed));
    row
}

/// `GET /plans/:id/plates`: Node's `readAcceptedPlateWorkspace`.
fn read(tx: &Transaction<'_>, tenant: &str, profile: i64, repos: &Path) -> Result<Response> {
    let ok = |body| Ok(Response { status: 200, body });
    if !visible(tx, tenant, profile)? {
        return Ok(Refusal::ProfileNotFound.response());
    }
    let snapshot = match accepted(tx, tenant, profile, repos)? {
        Ok(snapshot) => snapshot,
        Err(AcceptedRead::Empty { .. }) => return ok(json!({"kind": "empty_plan"})),
        Err(AcceptedRead::Missing) => return Ok(Refusal::ProfileNotFound.response()),
        Err(AcceptedRead::CompatibilityDirty | AcceptedRead::Uninitialized) => {
            return Ok(Refusal::StateUnavailable.response());
        }
        Err(AcceptedRead::IntegrityFailure { .. } | AcceptedRead::Ready { .. }) => {
            return Ok(inconsistent());
        }
    };
    let basis = Basis::of(&snapshot);
    let units = required(&snapshot);
    let by_token: HashMap<&str, _> = units
        .iter()
        .map(|(part, unit)| (unit.token.as_str(), (*part, *unit)))
        .collect();
    let stored = stored_layout(tx, tenant, profile, &basis, &snapshot)?;
    if units.is_empty() {
        return ok(json!({"kind": "empty_plan"}));
    }
    let printers: Vec<Value> = {
        let mut printers: Vec<Printer> = fleet(tx, tenant)?
            .into_iter()
            .filter_map(|(_, printer)| printer)
            .collect();
        printers.sort_by(|left, right| left.id.as_bytes().cmp(right.id.as_bytes()));
        printers.iter().map(Printer::json).collect()
    };
    let (revision, plates) = match stored {
        Stored::Setup(expected) => {
            return ok(json!({
                "kind": "setup",
                "basis": basis.json(),
                "expected_plate_revision_id": expected,
                "printers": printers,
                "units": units.iter().map(|(part, unit)| setup_unit(part, unit)).collect::<Vec<_>>(),
            }));
        }
        Stored::Ready(revision, plates) => (revision, plates),
    };
    let (views, unplaced, placed) = plate_views(&plates, &by_token)?;
    ok(json!({
        "kind": "ready",
        "basis": basis.json(),
        "plate_revision_id": revision.id,
        "plate_revision_number": revision.number,
        "arrange_undo_revision_id": revision.undo_from,
        "printers": printers,
        "plates": views,
        "unplaced": unplaced,
        "unassigned": units
            .iter()
            .filter(|(_, unit)| !placed.contains(&unit.token))
            .map(|(part, unit)| setup_unit(part, unit))
            .collect::<Vec<_>>(),
    }))
}

enum Stored {
    /// The setup workspace, expecting this head (`None` before any revision).
    Setup(Option<i64>),
    Ready(Revision, Vec<ValidatedPlate>),
}

/// The head revision's layout when it stamps `basis`; otherwise the head
/// id the setup workspace expects (`None` before any Plate revision).
fn stored_layout(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    basis: &Basis,
    snapshot: &Snapshot,
) -> Result<Stored> {
    Ok(match head(tx, tenant, profile)? {
        None => Stored::Setup(None),
        Some(id) => {
            let revision =
                revision(tx, tenant, profile, id)?.ok_or_else(|| anyhow!(Integrity("head")))?;
            if !basis.stamps(&revision) {
                Stored::Setup(Some(id))
            } else {
                let counted = |count: i64| (1..=i64::from(MAX_PLATE_UM)).contains(&count);
                ensure!(
                    counted(revision.plate_count)
                        && counted(revision.unit_count)
                        && Digest::parse(revision.layout_digest.as_str()).is_ok(),
                    Integrity("revision")
                );
                let plates = layout(tx, tenant, &revision, &tokens(snapshot)?)?;
                Stored::Ready(revision, plates)
            }
        }
    })
}

type Views = (Vec<Value>, Vec<Map<String, Value>>, HashSet<String>);

/// Node's `plateView` and `unplacedWorkspaceUnits`: each Plate with its
/// placed units, the unplaced units, and every token on a Plate.
fn plate_views(
    plates: &[ValidatedPlate],
    by_token: &HashMap<&str, (&graph::Part, &graph::Unit)>,
) -> Result<Views> {
    let mut views = Vec::new();
    let mut unplaced = Vec::new();
    let mut placed = HashSet::new();
    for plate in plates {
        let input = &plate.input;
        let plate_id =
            PlateId::parse(input.plate_id.as_str()).map_err(|_| anyhow!(Integrity("layout")))?;
        let mut members = Vec::new();
        for unit in &input.units {
            placed.insert(unit.token.as_str().to_owned());
            let (part, unit_row) = by_token
                .get(unit.token.as_str())
                .ok_or_else(|| anyhow!(Integrity("layout")))?;
            let mut row = setup_unit(part, unit_row);
            if unit.placement == Placement::Unplaced {
                row.insert("plate_id".into(), json!(plate_id.as_str()));
                row.insert("printer_id".into(), json!(input.printer_id));
            } else {
                row.insert("x_um".into(), json!(unit.x_um.get()));
                row.insert("y_um".into(), json!(unit.y_um.get()));
            }
            row.insert("width_um".into(), json!(unit.width_um.get()));
            row.insert("depth_um".into(), json!(unit.depth_um.get()));
            row.insert("height_um".into(), json!(unit.height_um.get()));
            if unit.placement == Placement::Unplaced {
                unplaced.push(row);
            } else {
                row.insert("placement".into(), json!(placement(unit.placement)));
                row.insert("pinned".into(), json!(unit.pinned));
                members.push(row);
            }
        }
        let geometry = &input.printer;
        views.push(json!({
            "plate_id": plate_id.as_str(),
            "ordinal": plate.ordinal,
            "printer": {
                "id": input.printer_id,
                "name": input.printer_name,
                "model": input.printer_model,
                "bed_width_um": geometry.bed_width_um.get(),
                "bed_depth_um": geometry.bed_depth_um.get(),
                "bed_height_um": geometry.bed_height_um.get(),
                "margin_um": geometry.margin_um.get(),
            },
            "units": members,
        }));
    }
    Ok((views, unplaced, placed))
}
