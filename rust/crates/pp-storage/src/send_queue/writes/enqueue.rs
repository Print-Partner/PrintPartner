//! Add to queue (GRE-1227): Node's `POST /printer-send-queue` form and its
//! `enqueuePrinterSend`. Adding only writes a `queued` row for a file the
//! route already staged; nothing is sent until Send now or Send ready.

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
}

const NOT_SLICED: &str = "Only .gcode / .bgcode / .gco files can be sent to a printer";
const NO_PLAN: &str = "Pick a plan to bind this send (profile_id required)";

impl NewItem {
    /// Node's `parsePrinterUploadMultipart` with the queue fields, then the
    /// route's Plan check. `file` is `None` without a file part, and
    /// `Some(None)` for a file part without a file name.
    pub fn parse(
        fields: &[(String, String)],
        file: Option<Option<&str>>,
    ) -> Result<Self, &'static str> {
        let (mut printer_id, mut plan, mut units) = ("", None, "");
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
                "checkoff_units" => units = value,
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
        Ok(Self {
            printer_id: printer_id.to_owned(),
            filename,
            plan,
            matched,
            wait_for_idle,
            start,
            units: checkoff_units(units),
        })
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
/// `part_id:unit_index` once; anything else is no units.
fn checkoff_units(raw: &str) -> String {
    let units: Vec<Value> = match serde_json::from_str(raw) {
        Ok(Value::Array(units)) => units,
        _ => Vec::new(),
    };
    let mut seen = HashSet::new();
    let units: Vec<Value> = units
        .iter()
        .filter_map(super::super::unit)
        .filter(|unit| seen.insert((unit["part_id"].clone(), unit["unit_index"].clone())))
        .collect();
    Value::Array(units).to_string()
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
            new.start,
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
}
