use super::{
    PresentedCredential, ProductMcp, ToolResult,
    pending_actions::Decision,
    write_result::{Code, Refusal, read_failure, writer_failure},
};
use pp_services::mcp_domain::ProposedAction;
use pp_storage::filaments::{NamedColor, Outcome, Request};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::cmp::Reverse;

pub(super) const MATCH: &str = "find_filament_matches";
pub(super) const ADD: &str = "propose_add_custom_filament";

pub(super) fn describe(tools: &mut Value) {
    if let Some(tools) = tools["tools"].as_array_mut()
        && let Some(tool) = tools.iter_mut().find(|tool| tool["name"] == MATCH)
    {
        tool["description"] = json!(
            "Find bundled and custom catalog colors by brand and name first, then by color. Does not check stock."
        );
    }
}

#[derive(Deserialize, Serialize)]
struct CatalogColor {
    id: String,
    display_name: String,
    #[serde(default)]
    product_line: String,
    #[serde(default)]
    hex: String,
    #[serde(default)]
    combo_label: String,
    #[serde(default)]
    swatch_url: String,
}

#[derive(Serialize)]
struct Ranked {
    #[serde(flatten)]
    entry: CatalogColor,
    exact_name: bool,
    name_match: bool,
    brand_match: bool,
    color_distance: Option<f64>,
}

struct Query {
    name: String,
    brand: String,
    color: Option<[u8; 3]>,
}

impl Query {
    fn parse(args: &Value) -> Result<Self, &'static str> {
        let name = text(args, "name").trim().to_lowercase();
        if name.is_empty() {
            return Err("name required");
        }
        let hex = text(args, "color_hex").trim();
        let color = if hex.is_empty() {
            None
        } else {
            Some(
                hex.strip_prefix('#')
                    .and_then(rgb)
                    .ok_or("color_hex must be a six-digit hex color")?,
            )
        };
        Ok(Self {
            name,
            brand: text(args, "brand").trim().to_lowercase(),
            color,
        })
    }

    fn rank(&self, colors: Vec<CatalogColor>) -> Value {
        let mut matches: Vec<_> = colors
            .into_iter()
            .filter_map(|entry| {
                let name = entry.display_name.to_lowercase();
                let exact_name = name == self.name;
                let name_match = name.contains(&self.name);
                let brand_match = !self.brand.is_empty()
                    && entry.product_line.to_lowercase().contains(&self.brand);
                let hex = entry.hex.trim();
                let color_distance = self
                    .color
                    .zip(rgb(hex.strip_prefix('#').unwrap_or(hex)))
                    .map(|(a, b)| {
                        a.into_iter()
                            .zip(b)
                            .map(|(a, b)| f64::from(a.abs_diff(b)).powi(2))
                            .sum::<f64>()
                            .sqrt()
                    });
                (name_match || brand_match || self.color.is_some()).then_some((
                    name,
                    Ranked {
                        entry,
                        exact_name,
                        name_match,
                        brand_match,
                        color_distance,
                    },
                ))
            })
            .collect();
        matches.sort_by(|(name_a, a), (name_b, b)| {
            let score = |m: &Ranked| {
                Reverse((
                    m.exact_name && m.brand_match,
                    m.exact_name,
                    m.brand_match,
                    m.name_match,
                ))
            };
            let distance = |m: &Ranked| m.color_distance.unwrap_or(f64::INFINITY);
            score(a)
                .cmp(&score(b))
                .then_with(|| distance(a).total_cmp(&distance(b)))
                .then_with(|| name_a.cmp(name_b))
                .then_with(|| a.entry.id.cmp(&b.entry.id))
        });
        let exact = matches
            .iter()
            .any(|(_, m)| m.exact_name && (self.brand.is_empty() || m.brand_match));
        json!({"matches": matches.into_iter().take(20).map(|(_, m)| m).collect::<Vec<_>>(), "exact_available": exact})
    }
}

fn rgb(hex: &str) -> Option<[u8; 3]> {
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let rgb = u32::from_str_radix(hex, 16).ok()?.to_be_bytes();
    Some([rgb[1], rgb[2], rgb[3]])
}

fn text<'a>(args: &'a Value, key: &str) -> &'a str {
    args[key].as_str().unwrap_or_default()
}

impl ProductMcp {
    pub(super) async fn find_filament_matches(
        &self,
        args: &Value,
        presented: PresentedCredential,
    ) -> ToolResult {
        let query = match Query::parse(args) {
            Ok(query) => query,
            Err(detail) => return ToolResult::error(json!({"error": detail})),
        };
        let client = self.reads.filaments.clone();
        let wait = self.reads.wait;
        match tokio::task::spawn_blocking(move || -> Result<Value, Refusal> {
            let custom = client
                .local_colors(presented.into_credential(), wait)
                .map_err(|error| writer_failure(&error))?;
            let error = || Refusal::new(Code::Internal, "The filament catalog could not be read.");
            let custom: Vec<Box<serde_json::value::RawValue>> =
                serde_json::from_str(&custom).map_err(|_| error())?;
            let mut catalog = crate::filaments::catalog_body().ok_or_else(error)?;
            let mut colors: Vec<CatalogColor> =
                serde_json::from_value(catalog["colors"].take()).map_err(|_| error())?;
            colors.extend(
                custom
                    .iter()
                    .filter_map(|entry| serde_json::from_str::<CatalogColor>(entry.get()).ok()),
            );
            Ok(query.rank(colors))
        })
        .await
        {
            Ok(Ok(matches)) => ToolResult::ok(matches.to_string()),
            Ok(Err(refusal)) => {
                ToolResult::error(json!({"error": refusal.detail, "code": refusal.code.as_str()}))
            }
            Err(_) => {
                ToolResult::error(json!({"error": "The filament catalog could not be read."}))
            }
        }
    }

    pub(super) async fn confirm_custom_filament(
        &self,
        action: ProposedAction,
        presented: PresentedCredential,
    ) -> Decision {
        let params = Value::Object(action.params);
        let Some(color) = NamedColor::new(
            text(&params, "display_name"),
            text(&params, "hex"),
            text(&params, "product_line"),
        ) else {
            return Decision::confirmed(
                Refusal::new(
                    Code::InvalidProposal,
                    "A name and six-digit RGB color are required.",
                )
                .confirmation(),
            );
        };
        let client = self.reads.filaments.clone();
        let wait = self.reads.wait;
        let body = match tokio::task::spawn_blocking(move || {
            client.submit(
                presented.into_credential(),
                Request::AddNamedColor(color),
                wait,
            )
        })
        .await
        {
            Ok(Ok(Outcome::NamedColor { filament, replay })) => json!({
                "ok": true, "result": {
                    "filament": filament,
                    "inventory_tracking": "external_named_color_only",
                    "idempotent_replay": replay,
                },
            }),
            Ok(Ok(Outcome::Refused(_, _, _))) => Refusal::new(
                Code::Internal,
                "The custom filament catalog cannot be updated. Nothing was written to it.",
            )
            .confirmation(),
            Ok(Err(error)) => {
                let refusal = read_failure(error);
                if !matches!(refusal.code, Code::Unauthorized | Code::Unavailable) {
                    return unknown();
                }
                refusal.confirmation()
            }
            _ => return unknown(),
        };
        Decision::confirmed(body)
    }
}

fn unknown() -> Decision {
    Decision::unknown(
        "The custom filament result is unknown. Check the catalog before proposing again.",
    )
}
