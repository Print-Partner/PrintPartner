//! Accepted Plan exports as pure functions. The caller reads the Plan and the
//! clock; nothing here touches storage, files or the network.

use std::collections::BTreeMap;

#[cfg(feature = "desktop_rebuild")]
pub mod pack;

/// The accepted Plan a checklist describes.
pub struct ChecklistPlan {
    pub build_name: String,
    pub order_number: Option<String>,
    pub plan_version: i64,
    /// Included Parts only.
    pub parts: Vec<ChecklistPart>,
}

/// One included Part of the accepted Plan.
pub struct ChecklistPart {
    /// The Source label, as `<layer>:<Source name>`.
    pub source: String,
    pub relative_path: String,
    pub filename: String,
    pub role: String,
    pub colour: Colour,
    /// Required units of the Part.
    pub quantity: u64,
    /// Required units checked off as printed.
    pub printed: u64,
    pub notes: String,
}

/// A Part's filament, as text and an optional swatch colour.
pub struct Colour {
    pub label: String,
    /// `#rgb` or `#rrggbb`, with or without the `#`. Anything else draws no
    /// swatch.
    pub hex: Option<String>,
}

/// A rendered checklist and the file name to download it as.
pub struct Checklist {
    pub filename: String,
    pub html: String,
}

/// The only thing a checklist may load: its own inline style sheet.
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; style-src 'unsafe-inline'";

/// Renders `plan` as one self-contained HTML page. Its Content Security
/// Policy allows only the inline style sheet, and it has no script, image or
/// link, so opening it from disk makes no request. All Plan text is escaped.
pub fn checklist(plan: &ChecklistPlan, generated_at: &str) -> Checklist {
    let mut groups: BTreeMap<(&str, &str), Vec<&ChecklistPart>> = BTreeMap::new();
    for part in &plan.parts {
        groups
            .entry((&part.source, folder(&part.relative_path)))
            .or_default()
            .push(part);
    }
    let printed: u64 = plan.parts.iter().map(|p| p.printed).sum();
    let total: u64 = plan.parts.iter().map(|p| p.quantity).sum();
    let name = escape(&plan.build_name);
    let mut html = format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta http-equiv=\"Content-Security-Policy\" content=\"{CONTENT_SECURITY_POLICY}\">\n\
         <title>{name} checklist, Plan version {version}</title>\n<style>{STYLE}</style>\n</head>\n<body>\n\
         <h1>{name}</h1>\n<dl>\n",
        version = plan.plan_version,
    );
    if let Some(order) = plan
        .order_number
        .as_deref()
        .filter(|o| !o.trim().is_empty())
    {
        html += &format!("<dt>Order number</dt><dd>{}</dd>\n", escape(order));
    }
    html += &format!(
        "<dt>Plan version</dt><dd>{}</dd>\n<dt>Generated</dt><dd>{}</dd>\n\
         <dt>Printed</dt><dd>{printed} of {total} units</dd>\n</dl>\n",
        plan.plan_version,
        escape(generated_at),
    );
    for ((source, folder), mut parts) in groups {
        parts.sort_by(|a, b| a.filename.cmp(&b.filename));
        let folder = if folder.is_empty() {
            "(top level)"
        } else {
            folder
        };
        html += &format!(
            "<h2>{} <span class=\"folder\">{}</span></h2>\n<table>\n<thead><tr><th>File</th><th>Role</th>\
             <th>Colour</th><th>Quantity</th><th>Printed</th><th>Notes</th></tr></thead>\n<tbody>\n",
            escape(source),
            escape(folder),
        );
        for part in parts {
            html += &row(part);
        }
        html += "</tbody>\n</table>\n";
    }
    html += "</body>\n</html>\n";
    Checklist {
        filename: format!(
            "{}-v{}-checklist.html",
            slug(&plan.build_name),
            plan.plan_version
        ),
        html,
    }
}

const STYLE: &str = "body{font-family:system-ui,sans-serif;margin:2rem;color:#111}\
table{border-collapse:collapse;width:100%;margin-bottom:1.5rem}\
th,td{border:1px solid #ccc;padding:.3rem .5rem;text-align:left;vertical-align:top}\
td.n{text-align:right}.folder{color:#555;font-weight:normal}\
.swatch{display:inline-block;width:.9em;height:.9em;border:1px solid #333;margin-right:.3em;vertical-align:middle}\
.notes{white-space:pre-wrap}dt{font-weight:bold}dd{margin:0 0 .3rem 0}";

fn row(part: &ChecklistPart) -> String {
    let swatch = part
        .colour
        .hex
        .as_deref()
        .and_then(swatch_hex)
        .map(|hex| format!("<span class=\"swatch\" style=\"background:{hex}\"></span>"))
        .unwrap_or_default();
    format!(
        "<tr><td>{}</td><td>{}</td><td>{swatch}{}</td><td class=\"n\">{}</td>\
         <td class=\"n\">{} / {}</td><td class=\"notes\">{}</td></tr>\n",
        escape(&part.filename),
        escape(&part.role),
        escape(&part.colour.label),
        part.quantity,
        part.printed,
        part.quantity,
        escape(&part.notes),
    )
}

/// The folder of a Source-relative path: everything before the last `/`.
fn folder(relative_path: &str) -> &str {
    relative_path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// `#` and 3 or 6 lowercase hex digits, or `None`. Only this reaches a
/// `style` attribute.
fn swatch_hex(hex: &str) -> Option<String> {
    let digits = hex.strip_prefix('#').unwrap_or(hex);
    (matches!(digits.len(), 3 | 6) && digits.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| format!("#{}", digits.to_ascii_lowercase()))
}

/// The Build name as a file name part: lowercase ASCII letters and digits,
/// other runs as one `-`, at most 60 bytes, `build` when nothing is left.
pub fn slug(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.truncate(60);
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        "build".into()
    } else {
        slug.into()
    }
}

fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            c => escaped.push(c),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(source: &str, path: &str, notes: &str, hex: Option<&str>) -> ChecklistPart {
        ChecklistPart {
            source: source.into(),
            relative_path: path.into(),
            filename: path.rsplit('/').next().unwrap().into(),
            role: "primary".into(),
            colour: Colour {
                label: "Galaxy Black".into(),
                hex: hex.map(Into::into),
            },
            quantity: 3,
            printed: 1,
            notes: notes.into(),
        }
    }

    fn plan(parts: Vec<ChecklistPart>) -> ChecklistPlan {
        ChecklistPlan {
            build_name: "Mk4 <Kit> & Co".into(),
            order_number: Some("A-17".into()),
            plan_version: 4,
            parts,
        }
    }

    #[test]
    fn names_the_file_by_build_and_version_and_states_the_header() {
        let out = checklist(&plan(vec![]), "2026-11-12T12:00:00Z");
        assert_eq!(out.filename, "mk4-kit-co-v4-checklist.html");
        for text in [
            "<h1>Mk4 &lt;Kit&gt; &amp; Co</h1>",
            "<dd>A-17</dd>",
            "<dt>Plan version</dt><dd>4</dd>",
            "<dd>2026-11-12T12:00:00Z</dd>",
            "<dd>0 of 0 units</dd>",
        ] {
            assert!(out.html.contains(text), "{text} missing");
        }
    }

    #[test]
    fn plan_text_is_escaped_and_the_page_loads_nothing() {
        let note = "<script>fetch('http://x/')</script> \"q\"";
        let hostile = part(
            "base:S\"<",
            "a/b.stl",
            note,
            Some("red;background:url(http://x)"),
        );
        let html = checklist(&plan(vec![hostile]), "t").html;
        assert!(
            html.contains("&lt;script&gt;fetch(&#39;http://x/&#39;)&lt;/script&gt; &quot;q&quot;")
        );
        for forbidden in [
            "<script", "src=", "href=", "url(", "@import", "<link", "<img",
        ] {
            assert!(!html.contains(forbidden), "{forbidden} in output");
        }
        assert!(html.contains("default-src 'none'; style-src 'unsafe-inline'"));
    }

    #[test]
    fn groups_by_source_then_folder_with_counts() {
        let parts = vec![
            part("base:Kit", "frame/z.stl", "", Some("#ABCDEF")),
            part("addon:Mods", "fan.stl", "", None),
            part("base:Kit", "frame/a.stl", "", Some("1A2")),
            part("base:Kit", "fan.stl", "", Some("#12345")),
        ];
        let html = checklist(&plan(parts), "t").html;
        let order: Vec<usize> = [
            "addon:Mods <span class=\"folder\">(top level)",
            "base:Kit <span class=\"folder\">(top level)",
            "base:Kit <span class=\"folder\">frame",
            "<td>a.stl",
            "<td>z.stl",
        ]
        .iter()
        .map(|text| html.find(text).unwrap_or_else(|| panic!("{text} missing")))
        .collect();
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
        assert!(html.contains("background:#abcdef"));
        assert!(html.contains("background:#1a2"));
        assert_eq!(
            html.matches("class=\"swatch\"").count(),
            2,
            "bad hex draws none"
        );
        assert!(html.contains("<td class=\"n\">1 / 3</td>"));
        assert!(html.contains("<dd>4 of 12 units</dd>"));
    }

    #[test]
    fn slug_keeps_ascii_words() {
        assert_eq!(slug("  Voron 2.4 — R2!  "), "voron-2-4-r2");
        assert_eq!(slug("ÆØÅ"), "build");
        assert_eq!(slug(&"a".repeat(80)).len(), 60);
    }
}
