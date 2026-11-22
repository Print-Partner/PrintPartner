//! Add to queue (GRE-1227): Node's `POST /printer-send-queue` form and its
//! `enqueuePrinterSend`. Adding only writes a `queued` row for a file the
//! route already staged; nothing is sent until Send now or Send ready.

use super::super::units::{LinkedUnits, SendUnits, StaleUnits};
use super::{Reply, farm_refusal, item};
use crate::read_model::{farm::iso_time, views::js_trim};
use anyhow::{Result, anyhow};
use rusqlite::{Transaction, params};
use serde_json::Value;
use std::{collections::HashSet, time::SystemTime};

/// The form fields Node's route reads, in its refusal order.
#[derive(Debug, PartialEq)]
pub struct NewItem {
    pub printer_id: String,
    /// The file part's base name, as stored and shown.
    pub filename: String,
    plan: i64,
    matched: &'static str,
    wait_for_idle: bool,
    start: bool,
    /// Normalized `checkoff_units` as JSON text.
    units: String,
    /// The preview binding the form sent with `link_checkoff` and no
    /// `checkoff_units` of its own: the route links the units the file
    /// prints, and only while they are still the units that preview named.
    link_checkoff: Option<String>,
}

const NOT_SLICED: &str = "Only .gcode / .bgcode / .gco files can be sent to a printer";
const NO_PLAN: &str = "Pick a plan to bind this send (profile_id required)";
pub const BAD_UNITS: &str = "checkoff_units must be a JSON array of at most 64 KiB";
const TOO_MANY_UNITS: &str =
    "checkoff_units links more units than one send can carry (1,000 units in 48 KiB)";
const NO_BINDING: &str = "link_checkoff needs the checkoff_binding its preview answered";

impl NewItem {
    /// Node's `parsePrinterUploadMultipart` with the queue fields, then the
    /// route's Plan check. `file` is `None` without a file part, and
    /// `Some(None)` for a file part without a file name.
    pub fn parse(
        fields: &[(String, String)],
        file: Option<Option<&str>>,
    ) -> Result<Self, &'static str> {
        let (mut printer_id, mut plan, mut units) = ("", None, None);
        let (mut link_checkoff, mut binding) = (false, "");
        let (mut start, mut wait_for_idle, mut matched) = (false, true, "pinned");
        for (name, value) in fields {
            let lower = value.to_lowercase();
            match name.as_str() {
                "printer_id" => printer_id = js_trim(value),
                "start" => start = matches!(lower.as_str(), "1" | "true" | "yes"),
                "wait_for_idle" => wait_for_idle = !matches!(lower.as_str(), "0" | "false" | "no"),
                "match" => match js_trim(&lower) {
                    "compatible" => matched = "compatible",
                    "pinned" => matched = "pinned",
                    _ => {}
                },
                "profile_id" | "plan_id" => plan = positive(value).or(plan),
                "checkoff_units" => units = Some(value.as_str()),
                "link_checkoff" => link_checkoff = matches!(lower.as_str(), "1" | "true" | "yes"),
                "checkoff_binding" => binding = js_trim(value),
                _ => {}
            }
        }
        if printer_id.is_empty() {
            return Err("printer_id is required");
        }
        let Some(file) = file else {
            return Err("G-code file required");
        };
        let raw = file
            .filter(|name| !name.is_empty())
            .unwrap_or("print.gcode")
            .replace('\\', "/");
        if !sliced(&staged_name(&raw)) {
            return Err(NOT_SLICED);
        }
        let filename = js_trim(basename(&raw)).to_owned();
        let plan = plan.ok_or(NO_PLAN)?;
        if filename.is_empty() {
            return Err("Could not enqueue item");
        }
        // Only an omitted field means no units. A blank or broken one is refused rather
        // than stored as none; the route refuses a field cut at the form's limit first.
        let explicit = units.is_some();
        let units = match units.map(str::trim) {
            Some(raw) => match serde_json::from_str::<Value>(raw) {
                Ok(Value::Array(units)) => checkoff_units(&units),
                _ => return Err(BAD_UNITS),
            },
            None => checkoff_units(&[]),
        };
        if !units.fits() {
            return Err(TOO_MANY_UNITS);
        }
        let link_checkoff = match (link_checkoff && !explicit, binding) {
            (false, _) => None,
            (true, "") => return Err(NO_BINDING),
            (true, binding) => Some(binding.to_owned()),
        };
        Ok(Self {
            printer_id: printer_id.to_owned(),
            filename,
            plan,
            matched,
            wait_for_idle,
            start,
            link_checkoff,
            units: units.json,
        })
    }

    /// The Build this send is bound to.
    pub fn plan(&self) -> i64 {
        self.plan
    }

    /// Whether the route should link the units the file prints, read from the
    /// staged file by [`super::super::units::send_units`].
    pub fn links_checkoff(&self) -> bool {
        self.link_checkoff.is_some()
    }

    /// Links the units `linked` names, when they are still the units the
    /// preview named: `linked` was read now, from the bytes being queued.
    pub fn link(&mut self, linked: &SendUnits) -> Result<(), StaleUnits> {
        if self.link_checkoff.as_deref() != Some(linked.binding.as_str()) {
            return Err(StaleUnits);
        }
        self.units = linked.json.clone();
        Ok(())
    }

    /// The name the staged file gets on disk, Node's
    /// `sanitizePrinterUploadFilename`.
    pub fn staged_name(&self) -> String {
        staged_name(&self.filename)
    }
}

/// Node's `path.basename` for a `/`-separated name.
fn basename(name: &str) -> &str {
    let name = name.trim_end_matches('/');
    name.rsplit('/').next().unwrap_or_default()
}

/// Node's `sanitizePrinterUploadFilename`, and `:` replaced too, so a
/// Windows drive prefix such as `C:x.gcode` stays one file name inside the
/// staging directory.
fn staged_name(name: &str) -> String {
    let base = js_trim(basename(name));
    if base.is_empty() || base.chars().all(|character| character == '.') {
        return "print.gcode".into();
    }
    base.replace(':', "_")
}

fn sliced(name: &str) -> bool {
    let lower = name.to_lowercase();
    [".gcode", ".bgcode", ".gco"]
        .iter()
        .any(|extension| lower.ends_with(extension))
}

/// JavaScript's `Number(value)` when it is a positive whole number.
fn positive(value: &str) -> Option<i64> {
    let number: f64 = js_trim(value).parse().ok()?;
    (number.fract() == 0.0 && number > 0.0 && number <= 9_007_199_254_740_991.0)
        .then_some(number as i64)
}

/// Node's `parseCheckoffUnits`: the valid units of a JSON array, each
/// `part_id:unit_index` once, with object names trimmed and cut at 200
/// UTF-16 units on a whole character.
pub(crate) fn checkoff_units(units: &[Value]) -> LinkedUnits {
    let mut seen = HashSet::new();
    let units: Vec<Value> = units
        .iter()
        .filter_map(super::super::unit)
        .filter(|unit| seen.insert((unit["part_id"].clone(), unit["unit_index"].clone())))
        .collect();
    LinkedUnits {
        count: units.len(),
        json: Value::Array(units).to_string(),
    }
}

/// Writes `new` as the queue's last `queued` item, after the Plan exists and
/// Farm is on for it.
pub(super) fn add(
    tx: &Transaction<'_>,
    tenant: &str,
    new: &NewItem,
    artifact_path: &str,
    host_name: &str,
) -> Result<Reply> {
    let plan = pp_domain::PositiveId::new(new.plan.unsigned_abs()).map_err(anyhow::Error::msg)?;
    if !crate::features::owned(tx, tenant, plan)? {
        return Ok(Reply::Refused {
            status: 404,
            detail: "Profile not found",
            code: None,
        });
    }
    if let Some(refusal) = farm_refusal(tx, tenant, Some(new.plan))? {
        return Ok(refusal);
    }
    let id = crate::checkoff_progress::links::uuid();
    let now = iso_time(SystemTime::now());
    tx.execute(
        "INSERT INTO printer_send_queue(tenant_id,position,id,filename,artifact_path,printer_id,profile_id,match,wait_for_idle,start,state,checkoff_units,host_name,created_at,updated_at)
         VALUES(?1,(SELECT COALESCE(MAX(position)+1,0) FROM printer_send_queue WHERE tenant_id=?1),?2,?3,?4,?5,?6,?7,?8,?9,'queued',?10,?11,?12,?12)",
        params![
            tenant,
            id,
            new.filename,
            js_trim(artifact_path),
            new.printer_id,
            new.plan,
            new.matched,
            new.wait_for_idle,
            new.start && !super::super::upload_only(tx, tenant)?,
            new.units,
            Some(js_trim(host_name)).filter(|name| !name.is_empty()),
            now,
        ],
    )?;
    let item = item(tx, tenant, &id)?.ok_or_else(|| anyhow!("Added item vanished"))?;
    Ok(Reply::Item(item))
}

#[cfg(test)]
mod tests {
    use super::NewItem;

    fn fields(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn the_form_is_refused_in_node_order() {
        let parse = |pairs: &[(&str, &str)], file| NewItem::parse(&fields(pairs), file);
        assert_eq!(
            parse(&[], Some(Some("a.gcode"))),
            Err("printer_id is required")
        );
        assert_eq!(
            parse(&[("printer_id", " p ")], None),
            Err("G-code file required")
        );
        let printer = [("printer_id", "p"), ("profile_id", "3")];
        for name in ["a.stl", "a.3mf", "a.gcode.zip", "a.GCODE.txt"] {
            assert_eq!(
                parse(&printer, Some(Some(name))).map(drop),
                Err(super::NOT_SLICED),
                "{name}"
            );
        }
        assert_eq!(
            parse(&[("printer_id", "p")], Some(Some("a.gcode"))),
            Err(super::NO_PLAN)
        );
        assert_eq!(
            parse(
                &[("printer_id", "p"), ("profile_id", "0")],
                Some(Some("a.gco"))
            ),
            Err(super::NO_PLAN)
        );
        assert_eq!(
            parse(
                &[("printer_id", "p"), ("profile_id", "1.5")],
                Some(Some("a.gco"))
            ),
            Err(super::NO_PLAN)
        );
    }

    #[test]
    fn the_form_keeps_node_defaults_and_base_names() {
        let form = fields(&[("printer_id", " p-a "), ("plan_id", "7")]);
        let item = NewItem::parse(&form, Some(Some("C:\\prints\\Plate 1.BGCODE"))).unwrap();
        assert_eq!(item.printer_id, "p-a");
        assert_eq!(item.filename, "Plate 1.BGCODE");
        assert_eq!(item.staged_name(), "Plate 1.BGCODE");
        assert_eq!(
            (item.plan, item.matched, item.wait_for_idle, item.start),
            (7, "pinned", true, false)
        );
        assert_eq!(item.units, "[]");

        for (raw, staged) in [
            ("../../x.gcode", "x.gcode"),
            ("..\\..\\x.gcode", "x.gcode"),
            ("C:x.gcode", "C_x.gcode"),
            ("a/../", "print.gcode"),
        ] {
            let item = NewItem::parse(&form, Some(Some(raw))).unwrap();
            assert_eq!(item.staged_name(), staged, "{raw}");
        }
        for unnamed in [None, Some("")] {
            assert_eq!(
                NewItem::parse(&form, Some(unnamed)).unwrap().filename,
                "print.gcode"
            );
        }

        let form = fields(&[
            ("printer_id", "p"),
            ("profile_id", "2"),
            ("start", "YES"),
            ("wait_for_idle", "no"),
            ("match", " Compatible "),
            ("match", "neither"),
            (
                "checkoff_units",
                r#"[{"part_id":4,"unit_index":0,"object_name":" A "},{"part_id":4,"unit_index":0},{"part_id":0,"unit_index":1}]"#,
            ),
        ]);
        let item = NewItem::parse(&form, Some(Some("a.gcode"))).unwrap();
        assert_eq!(
            (item.matched, item.wait_for_idle, item.start),
            ("compatible", false, true)
        );
        let units: serde_json::Value = serde_json::from_str(&item.units).unwrap();
        assert_eq!(
            units,
            serde_json::json!([{ "part_id": 4, "unit_index": 0, "object_name": "A" }])
        );
    }

    #[test]
    fn gre_1896_units_that_are_blank_or_not_a_json_array_are_refused() {
        let cut = format!(
            r#"[{{"part_id":4,"unit_index":0,"object_name":"{}"#,
            "x".repeat(64)
        );
        for raw in ["{}", "nope", cut.as_str(), "", "   "] {
            let form = fields(&[
                ("printer_id", "p"),
                ("profile_id", "2"),
                ("checkoff_units", raw),
            ]);
            assert_eq!(
                NewItem::parse(&form, Some(Some("a.gcode"))),
                Err(super::BAD_UNITS),
                "{raw:?}"
            );
        }
        let form = fields(&[("printer_id", "p"), ("profile_id", "2")]);
        assert_eq!(
            NewItem::parse(&form, Some(Some("a.gcode"))).unwrap().units,
            "[]"
        );
    }

    #[test]
    fn gre_1896_explicit_units_one_send_cannot_carry_are_refused() {
        let units: Vec<String> = (0..1001)
            .map(|i| format!(r#"{{"part_id":4,"unit_index":{i}}}"#))
            .collect();
        let raw = format!("[{}]", units.join(","));
        let form = fields(&[
            ("printer_id", "p"),
            ("profile_id", "2"),
            ("checkoff_units", &raw),
        ]);
        assert_eq!(
            NewItem::parse(&form, Some(Some("a.gcode"))),
            Err(super::TOO_MANY_UNITS)
        );
    }

    #[test]
    fn gre_1896_link_checkoff_needs_its_binding_and_no_explicit_units() {
        let parse = |pairs: &[(&str, &str)]| {
            let mut form = vec![("printer_id", "p"), ("profile_id", "2")];
            form.extend_from_slice(pairs);
            NewItem::parse(&fields(&form), Some(Some("a.gcode"))).map(|item| item.links_checkoff())
        };
        assert_eq!(
            parse(&[("link_checkoff", "1"), ("checkoff_binding", "b")]),
            Ok(true)
        );
        assert_eq!(parse(&[("link_checkoff", "1")]), Err(super::NO_BINDING));
        assert_eq!(
            parse(&[("link_checkoff", "0"), ("checkoff_binding", "b")]),
            Ok(false)
        );
        assert_eq!(parse(&[]), Ok(false));
        let explicit = [("link_checkoff", "1"), ("checkoff_units", "[]")];
        assert_eq!(parse(&explicit), Ok(false));
    }
}
