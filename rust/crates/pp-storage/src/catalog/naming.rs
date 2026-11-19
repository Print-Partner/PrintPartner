use super::*;
use sha2::{Digest, Sha256};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamingProfile {
    pub roles: Vec<Role>,
    pub quantity: Quantity,
    pub slug: Slug,
    pub folder_rules: Vec<FolderRule>,
    pub export_role_order: Vec<RoleId>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoleId {
    Primary,
    Accent,
    Clear,
    Opaque,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Role {
    pub id: RoleId,
    pub label: String,
    pub markers: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Quantity {
    pub regex: String,
    pub default: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Slug {
    pub strip_markers: bool,
    pub strip_quantity: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FolderRule {
    pub path_contains: String,
    pub role_id: RoleId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub functional_class: Option<FunctionalClass>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FunctionalClass {
    Functional,
    Cosmetic,
}
#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NamingCommand {
    UseDefaults,
    Override { profile: NamingProfile },
}
impl Default for NamingProfile {
    fn default() -> Self {
        Self {
            roles: vec![
                Role {
                    id: RoleId::Primary,
                    label: "Primary".into(),
                    markers: vec![],
                },
                Role {
                    id: RoleId::Accent,
                    label: "Accent".into(),
                    markers: vec!["[a]".into()],
                },
                Role {
                    id: RoleId::Clear,
                    label: "Clear".into(),
                    markers: vec!["[c]".into()],
                },
                Role {
                    id: RoleId::Opaque,
                    label: "Opaque".into(),
                    markers: vec!["[o]".into()],
                },
            ],
            quantity: Quantity {
                regex: "[ _]x([0-9]+)\\.stl$".into(),
                default: 1,
            },
            slug: Slug {
                strip_markers: true,
                strip_quantity: true,
            },
            folder_rules: vec![],
            export_role_order: vec![
                RoleId::Primary,
                RoleId::Accent,
                RoleId::Clear,
                RoleId::Opaque,
            ],
        }
    }
}
impl NamingProfile {
    pub(super) fn validate_input(self) -> Result<Self> {
        let validated = self.validate();
        #[cfg(feature = "desktop_rebuild")]
        let validated = validated.map_err(|error| {
            let detail = error.to_string();
            error.context(CatalogFailure::Input(detail))
        });
        validated
    }
    pub(super) fn validate(mut self) -> Result<Self> {
        ensure!(
            !self.roles.is_empty() && self.roles.iter().any(|r| r.id == RoleId::Primary),
            "Naming requires primary role"
        );
        let mut seen = Vec::new();
        for r in &mut self.roles {
            ensure!(!seen.contains(&r.id), "Duplicate naming role");
            seen.push(r.id);
            r.label = trim(&r.label).into();
            ensure!(!r.label.is_empty(), "Blank naming label");
        }
        ensure!(
            self.export_role_order.len() == 4,
            "Invalid export role order"
        );
        let mut seen = Vec::new();
        for id in &self.export_role_order {
            ensure!(!seen.contains(id), "Duplicate export role");
            seen.push(*id);
        }
        self.quantity.regex = trim(&self.quantity.regex).into();
        ensure!(
            self.quantity.default > 0 && self.quantity.default <= 9_007_199_254_740_991,
            "Invalid quantity default"
        );
        validate_regex_structure(&self.quantity.regex)
            .map_err(|refusal| anyhow!(refusal.message))?;
        #[cfg(feature = "desktop_rebuild")]
        ensure!(
            node_regex_syntax(&self.quantity.regex),
            "Invalid quantity regex"
        );
        regress::Regex::with_flags(&self.quantity.regex, "i")
            .map_err(|_| anyhow!("Invalid quantity regex"))?;
        let regex = regress::Regex::new(&format!("(?:{})|", self.quantity.regex))
            .map_err(|_| anyhow!("Invalid quantity regex"))?;
        ensure!(
            regex.find("").is_some_and(|m| m.captures.len() == 1),
            "Quantity regex must have one capture group"
        );
        for r in &mut self.folder_rules {
            r.path_contains = trim(&r.path_contains).into();
            ensure!(!r.path_contains.is_empty(), "Blank folder rule");
        }
        Ok(self)
    }
}
struct RegexRefusal {
    message: &'static str,
    /// The pattern is otherwise well formed, so only a Rust-only bound refused it.
    #[cfg(feature = "desktop_rebuild")]
    limit: bool,
}
/// Reports the first refusal, but scans on past a limit so `limit` is only set
/// for patterns whose groups and classes are otherwise balanced.
fn validate_regex_structure(pattern: &str) -> Result<(), RegexRefusal> {
    const MAX_GROUP_DEPTH: usize = 16;
    const MAX_GROUPS: usize = 64;
    let mut exceeded = (pattern.len() > 4096).then_some("Quantity regex too long");
    let refuse = |message, _limit: bool| {
        Err(RegexRefusal {
            message,
            #[cfg(feature = "desktop_rebuild")]
            limit: _limit,
        })
    };
    let mut escaped = false;
    let mut in_class = false;
    let mut depth = 0usize;
    let mut groups = 0usize;
    for byte in pattern.bytes() {
        if escaped {
            escaped = false;
            continue;
        }
        if byte == b'\\' {
            escaped = true;
            continue;
        }
        if in_class {
            if byte == b']' {
                in_class = false;
            }
            continue;
        }
        match byte {
            b'[' => in_class = true,
            b'(' => {
                depth += 1;
                groups += 1;
                if depth > MAX_GROUP_DEPTH || groups > MAX_GROUPS {
                    exceeded = exceeded.or(Some("Quantity regex exceeds group complexity limits"));
                }
            }
            b')' => {
                if depth == 0 {
                    return refuse(exceeded.unwrap_or("Invalid quantity regex groups"), false);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    if escaped || in_class || depth != 0 {
        return refuse(
            exceeded.unwrap_or("Invalid quantity regex structure"),
            false,
        );
    }
    match exceeded {
        Some(message) => refuse(message, true),
        None => Ok(()),
    }
}
/// Rejects ES2025 syntax `regress` accepts but Node 22's V8 does not: group
/// modifiers such as `(?i:a)` and group names repeated across alternatives.
#[cfg(feature = "desktop_rebuild")]
fn node_regex_syntax(pattern: &str) -> bool {
    let mut names = Vec::new();
    let mut escaped = false;
    let mut in_class = false;
    for (index, byte) in pattern.bytes().enumerate() {
        if escaped {
            escaped = false;
        } else if byte == b'\\' {
            escaped = true;
        } else if in_class {
            in_class = byte != b']';
        } else if byte == b'[' {
            in_class = true;
        } else if byte == b'('
            && let Some(group) = pattern[index + 1..].strip_prefix('?')
        {
            match group.bytes().next() {
                Some(b':' | b'=' | b'!') => {}
                Some(b'<') if group[1..].starts_with(['=', '!']) => {}
                Some(b'<') => {
                    let Some((name, _)) = group[1..].split_once('>') else {
                        return false;
                    };
                    let Some(name) = decode_group_name(name) else {
                        return false;
                    };
                    if names.contains(&name) {
                        return false;
                    }
                    names.push(name);
                }
                _ => return false,
            }
        }
    }
    true
}

#[cfg(feature = "desktop_rebuild")]
fn decode_group_name(name: &str) -> Option<Vec<u16>> {
    let mut units = Vec::new();
    let mut chars = name.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\\' {
            units.extend_from_slice(character.encode_utf16(&mut [0; 2]));
            continue;
        }
        if chars.next() != Some('u') {
            return None;
        }
        let mut code = 0u32;
        if chars.next_if_eq(&'{').is_some() {
            let mut digits = 0;
            loop {
                let digit = chars.next()?;
                if digit == '}' {
                    break;
                }
                code = code.checked_mul(16)?.checked_add(digit.to_digit(16)?)?;
                digits += 1;
            }
            if digits == 0 {
                return None;
            }
            units.extend_from_slice(char::from_u32(code)?.encode_utf16(&mut [0; 2]));
        } else {
            for _ in 0..4 {
                code = code * 16 + chars.next()?.to_digit(16)?;
            }
            units.push(code as u16);
        }
    }
    Some(units)
}

pub(crate) fn uses_defaults(m: &Map<String, Value>) -> bool {
    m.get("naming")
        .and_then(Value::as_object)
        .and_then(|n| n.get("use_defaults"))
        .and_then(Value::as_bool)
        .unwrap_or(true)
}
pub(crate) fn parse_global(raw: Option<String>) -> NamingProfile {
    raw.and_then(|s| serde_json::from_str::<NamingProfile>(&s).ok())
        .and_then(|p| p.validate().ok())
        .unwrap_or_default()
}
pub(super) fn global(tx: &Transaction<'_>, tenant: &str) -> Result<NamingProfile> {
    Ok(parse_global(get_setting(
        tx,
        tenant,
        "stl_naming_defaults",
    )?))
}
fn response(profile: NamingProfile, use_defaults: bool, override_value: Value) -> Result<Value> {
    #[derive(Serialize)]
    struct Canonical<'a> {
        version: u8,
        roles: &'a [Role],
        quantity: &'a Quantity,
        slug: &'a Slug,
        folder_rules: Vec<CanonicalFolder<'a>>,
        export_role_order: &'a [RoleId],
    }
    #[derive(Serialize)]
    struct CanonicalFolder<'a> {
        path_contains: &'a str,
        role_id: RoleId,
        functional_class: &'a Option<FunctionalClass>,
    }
    let canonical = Canonical {
        version: 1,
        roles: &profile.roles,
        quantity: &profile.quantity,
        slug: &profile.slug,
        folder_rules: profile
            .folder_rules
            .iter()
            .map(|f| CanonicalFolder {
                path_contains: &f.path_contains,
                role_id: f.role_id,
                functional_class: &f.functional_class,
            })
            .collect(),
        export_role_order: &profile.export_role_order,
    };
    let digest = hex::encode(Sha256::digest(serde_json::to_vec(&canonical)?));
    Ok(
        json!({"use_defaults":use_defaults,"override":override_value,"effective":profile,"effective_digest":digest}),
    )
}
pub(crate) fn get(tx: &Transaction<'_>, tenant: &str, id: i64) -> Result<Value> {
    let source = require_source(tx, tenant, id)?;
    let m = source.metadata.unwrap_or_default();
    let global = global(tx, tenant)?;
    let projected = project(m, global);
    #[cfg(feature = "desktop_rebuild")]
    let projected = projected.map_err(|error| error.context(CatalogFailure::InvalidStoredNaming));
    projected
}
fn require_source(tx: &Transaction<'_>, tenant: &str, id: i64) -> Result<SourceSummary> {
    #[cfg(feature = "desktop_rebuild")]
    {
        super::get(tx, tenant, id)?.ok_or_else(|| CatalogFailure::NotFound.into())
    }
    #[cfg(not(feature = "desktop_rebuild"))]
    require(tx, tenant, id)
}
pub(crate) fn project(m: Map<String, Value>, global: NamingProfile) -> Result<Value> {
    let Some(naming) = m.get("naming") else {
        return response(global, true, json!({}));
    };
    let Some(naming) = naming.as_object() else {
        return response(global, true, json!({}));
    };
    let defaults = naming
        .get("use_defaults")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if defaults {
        return response(global, true, json!({}));
    }
    let override_value = naming
        .get("override")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or(json!({}));
    #[cfg(feature = "desktop_rebuild")]
    validate_stored_override(&override_value)?;
    if let Some(full) = serde_json::from_value::<NamingProfile>(override_value.clone())
        .ok()
        .and_then(|p| p.validate().ok())
    {
        return response(full.clone(), false, serde_json::to_value(full)?);
    }
    merge_override(override_value, global)
}
pub(crate) fn merge_override(override_value: Value, global: NamingProfile) -> Result<Value> {
    #[cfg(feature = "desktop_rebuild")]
    validate_stored_override(&override_value)?;
    let mut effective = serde_json::to_value(global)?;
    let object = override_value
        .as_object()
        .ok_or_else(|| anyhow!("Invalid naming override"))?;
    for (k, v) in object {
        ensure!(
            matches!(
                k.as_str(),
                "roles" | "quantity" | "slug" | "folder_rules" | "export_role_order"
            ),
            "Unknown naming field"
        );
        if k == "roles" {
            let roles = v.as_array().ok_or_else(|| anyhow!("Invalid roles"))?;
            let base = effective["roles"]
                .as_array_mut()
                .ok_or_else(|| anyhow!("Naming roles are missing"))?;
            let mut seen = Vec::new();
            ensure!(!roles.is_empty(), "Empty roles override");
            for r in roles {
                let id = r.get("id").ok_or_else(|| anyhow!("Missing role id"))?;
                ensure!(!seen.contains(id), "Duplicate role override");
                seen.push(id.clone());
                serde_json::from_value::<RoleId>(id.clone())?;
                let index = if let Some(index) = base.iter().position(|b| b.get("id") == Some(id)) {
                    index
                } else {
                    base.push(json!({"id":id,"label":id,"markers":[]}));
                    base.len() - 1
                };
                let target = base
                    .get_mut(index)
                    .and_then(Value::as_object_mut)
                    .ok_or_else(|| anyhow!("Naming role override is missing"))?;
                target.extend(
                    r.as_object()
                        .ok_or_else(|| anyhow!("Invalid role"))?
                        .clone(),
                );
            }
        } else if matches!(k.as_str(), "quantity" | "slug") {
            effective[k]
                .as_object_mut()
                .ok_or_else(|| anyhow!("Invalid naming fields"))?
                .extend(
                    v.as_object()
                        .ok_or_else(|| anyhow!("Invalid naming fields"))?
                        .clone(),
                );
        } else {
            effective[k] = v.clone();
        }
    }
    let effective = serde_json::from_value::<NamingProfile>(effective)?.validate()?;
    let normalized = serde_json::to_value(&effective)?;
    let mut override_value = override_value;
    let override_object = override_value
        .as_object_mut()
        .ok_or_else(|| anyhow!("Invalid naming override"))?;
    for (key, value) in override_object {
        if key == "roles" {
            let roles = value
                .as_array_mut()
                .ok_or_else(|| anyhow!("Invalid roles"))?;
            let normalized_roles = normalized["roles"]
                .as_array()
                .ok_or_else(|| anyhow!("Naming roles are missing"))?;
            for role in roles {
                let id = role["id"].clone();
                let full = normalized_roles
                    .iter()
                    .find(|part| part["id"] == id)
                    .ok_or_else(|| anyhow!("Naming role override is missing"))?;
                for (field, field_value) in role
                    .as_object_mut()
                    .ok_or_else(|| anyhow!("Invalid role"))?
                {
                    *field_value = full[field].clone();
                }
            }
        } else if matches!(key.as_str(), "quantity" | "slug") {
            for (field, field_value) in value
                .as_object_mut()
                .ok_or_else(|| anyhow!("Invalid naming fields"))?
            {
                *field_value = normalized[key][field].clone();
            }
        } else {
            *value = normalized[key].clone();
        }
    }
    response(effective, false, override_value)
}

/// Mirrors the first issue Node's `StlNamingProfileOverrideSchema` reports:
/// fields in schema order, then array length and uniqueness, then unknown keys.
#[cfg(feature = "desktop_rebuild")]
fn validate_stored_override(value: &Value) -> Result<()> {
    const ROLE_IDS: &[&str] = &["primary", "accent", "clear", "opaque"];
    fn fail<T>(path: &str, message: impl std::fmt::Display) -> Result<T> {
        Err(anyhow!("{path}: {message}"))
    }
    fn received(value: Option<&Value>) -> &'static str {
        match value {
            None => "undefined",
            Some(Value::Null) => "null",
            Some(Value::Bool(_)) => "boolean",
            Some(Value::Number(_)) => "number",
            Some(Value::String(_)) => "string",
            Some(Value::Array(_)) => "array",
            Some(Value::Object(_)) => "object",
        }
    }
    fn invalid_type<T>(path: &str, expected: &str, value: Option<&Value>) -> Result<T> {
        let received = received(value);
        fail(
            path,
            format!("Invalid input: expected {expected}, received {received}"),
        )
    }
    fn object<'a>(value: Option<&'a Value>, path: &str) -> Result<&'a Map<String, Value>> {
        match value {
            Some(Value::Object(object)) => Ok(object),
            other => invalid_type(path, "object", other),
        }
    }
    fn array<'a>(value: Option<&'a Value>, path: &str) -> Result<&'a [Value]> {
        match value {
            Some(Value::Array(items)) => Ok(items),
            other => invalid_type(path, "array", other),
        }
    }
    fn nonblank<'a>(value: Option<&'a Value>, path: &str) -> Result<&'a str> {
        let Some(Value::String(text)) = value else {
            return invalid_type(path, "string", value);
        };
        let text = trim(text);
        if text.is_empty() {
            return fail(path, "must not be blank");
        }
        Ok(text)
    }
    fn option(value: Option<&Value>, path: &str, options: &[&str]) -> Result<()> {
        if value
            .and_then(Value::as_str)
            .is_some_and(|text| options.contains(&text))
        {
            return Ok(());
        }
        let options = options
            .iter()
            .map(|option| format!("\"{option}\""))
            .collect::<Vec<_>>()
            .join("|");
        fail(path, format!("Invalid option: expected one of {options}"))
    }
    fn strict(object: &Map<String, Value>, path: &str, known: &[&str]) -> Result<()> {
        // JavaScript enumerates array-index keys first, ascending, then the rest in order.
        let index = |key: &str| {
            key.parse::<u32>()
                .ok()
                .filter(|index| *index != u32::MAX && index.to_string() == key)
        };
        let mut unknown = object
            .keys()
            .filter(|key| !known.contains(&key.as_str()))
            .collect::<Vec<_>>();
        unknown.sort_by_key(|key| index(key).map_or((1, 0), |index| (0, index)));
        let unknown = unknown
            .into_iter()
            .map(|key| format!("\"{key}\""))
            .collect::<Vec<_>>();
        match unknown.len() {
            0 => Ok(()),
            1 => fail(path, format!("Unrecognized key: {}", unknown[0])),
            _ => fail(path, format!("Unrecognized keys: {}", unknown.join(", "))),
        }
    }
    fn unique(items: &[Value]) -> bool {
        items
            .iter()
            .enumerate()
            .all(|(index, item)| !items[..index].contains(item))
    }
    fn quantity_regex(value: Option<&Value>, path: &str) -> Result<Option<&'static str>> {
        let pattern = nonblank(value, path)?;
        if let Err(refusal) = validate_regex_structure(pattern) {
            return if refusal.limit {
                Ok(Some(refusal.message))
            } else {
                fail(path, "is an invalid regular expression")
            };
        }
        if !node_regex_syntax(pattern) || regress::Regex::with_flags(pattern, "i").is_err() {
            return fail(path, "is an invalid regular expression");
        }
        let groups = regress::Regex::new(&format!("(?:{pattern})|"))
            .ok()
            .and_then(|regex| regex.find("").map(|found| found.captures.len()));
        if groups != Some(1) {
            return fail(path, "must contain exactly one capture group");
        }
        Ok(None)
    }
    fn quantity_default(value: Option<&Value>, path: &str) -> Result<()> {
        const MAX_SAFE: f64 = 9_007_199_254_740_991.0;
        let Some(number) = value.and_then(Value::as_f64) else {
            return invalid_type(path, "number", value);
        };
        if number.fract() != 0.0 {
            invalid_type(path, "int", value)
        } else if number > MAX_SAFE {
            fail(path, "Too big: expected int to be <=9007199254740991")
        } else if number < -MAX_SAFE {
            fail(path, "Too small: expected int to be >=-9007199254740991")
        } else if number < 1.0 {
            fail(path, "Too small: expected number to be >=1")
        } else {
            Ok(())
        }
    }

    let root = object(Some(value), "override")?;
    if let Some(roles) = root.get("roles") {
        let roles = array(Some(roles), "override.roles")?;
        for (index, role) in roles.iter().enumerate() {
            let path = format!("override.roles.{index}");
            let role = object(Some(role), &path)?;
            option(role.get("id"), &format!("{path}.id"), ROLE_IDS)?;
            if let Some(label) = role.get("label") {
                nonblank(Some(label), &format!("{path}.label"))?;
            }
            if let Some(markers) = role.get("markers") {
                let markers = array(Some(markers), &format!("{path}.markers"))?;
                for (marker_index, marker) in markers.iter().enumerate() {
                    if !marker.is_string() {
                        return invalid_type(
                            &format!("{path}.markers.{marker_index}"),
                            "string",
                            Some(marker),
                        );
                    }
                }
            }
            strict(role, &path, &["id", "label", "markers"])?;
        }
        if roles.is_empty() {
            return fail("override.roles", "must contain at least one role override");
        }
        let ids = roles
            .iter()
            .map(|role| role["id"].clone())
            .collect::<Vec<_>>();
        if !unique(&ids) {
            return fail("override.roles", "role ids must be unique");
        }
    }
    let mut regex_limit = None;
    if let Some(quantity) = root.get("quantity") {
        let quantity = object(Some(quantity), "override.quantity")?;
        if let Some(regex) = quantity.get("regex") {
            regex_limit = quantity_regex(Some(regex), "override.quantity.regex")?;
        }
        if let Some(default) = quantity.get("default") {
            quantity_default(Some(default), "override.quantity.default")?;
        }
        strict(quantity, "override.quantity", &["regex", "default"])?;
    }
    if let Some(slug) = root.get("slug") {
        let slug = object(Some(slug), "override.slug")?;
        for field in ["strip_markers", "strip_quantity"] {
            if let Some(flag) = slug.get(field).filter(|flag| !flag.is_boolean()) {
                return invalid_type(&format!("override.slug.{field}"), "boolean", Some(flag));
            }
        }
        strict(slug, "override.slug", &["strip_markers", "strip_quantity"])?;
    }
    if let Some(rules) = root.get("folder_rules") {
        let rules = array(Some(rules), "override.folder_rules")?;
        for (index, rule) in rules.iter().enumerate() {
            let path = format!("override.folder_rules.{index}");
            let rule = object(Some(rule), &path)?;
            nonblank(rule.get("path_contains"), &format!("{path}.path_contains"))?;
            option(rule.get("role_id"), &format!("{path}.role_id"), ROLE_IDS)?;
            if let Some(class) = rule.get("functional_class") {
                option(
                    Some(class),
                    &format!("{path}.functional_class"),
                    &["functional", "cosmetic"],
                )?;
            }
            strict(
                rule,
                &path,
                &["path_contains", "role_id", "functional_class"],
            )?;
        }
    }
    if let Some(order) = root.get("export_role_order") {
        let path = "override.export_role_order";
        let order = array(Some(order), path)?;
        // zod checks an array's upper size bound before its elements.
        if order.len() > 4 {
            return fail(path, "Too big: expected array to have exactly 4 items");
        }
        for (index, id) in order.iter().enumerate() {
            option(Some(id), &format!("{path}.{index}"), ROLE_IDS)?;
        }
        if order.len() < 4 {
            return fail(path, "Too small: expected array to have exactly 4 items");
        }
        if !unique(order) {
            return fail(path, "must list each role id exactly once");
        }
    }
    strict(
        root,
        "override",
        &[
            "roles",
            "quantity",
            "slug",
            "folder_rules",
            "export_role_order",
        ],
    )?;
    match regex_limit {
        Some(message) => fail("override.quantity.regex", message),
        None => Ok(()),
    }
}
pub(super) fn save(
    tx: &Transaction<'_>,
    tenant: &str,
    id: i64,
    command: NamingCommand,
) -> Result<Value> {
    let mut m = require_source(tx, tenant, id)?.metadata.unwrap_or_default();
    let naming = match command {
        NamingCommand::UseDefaults => json!({"use_defaults":true,"override":{}}),
        NamingCommand::Override { profile } => {
            json!({"use_defaults":false,"override":profile.validate_input()?})
        }
    };
    m.insert("naming".into(), naming);
    save_metadata(tx, tenant, id, m)?;
    get(tx, tenant, id)
}

/// The file name without a `.stl` or `.3mf` extension, in any case.
#[cfg(feature = "desktop_rebuild")]
pub(crate) fn model_stem(filename: &str) -> Option<&str> {
    let split = filename.len().checked_sub(4)?;
    let extension = filename.get(split..)?;
    (extension.eq_ignore_ascii_case(".stl") || extension.eq_ignore_ascii_case(".3mf"))
        .then(|| &filename[..split])
}

/// Whether `filename` ends in `.3mf`, in any case.
#[cfg(feature = "desktop_rebuild")]
pub(crate) fn is_3mf(filename: &str) -> bool {
    filename
        .len()
        .checked_sub(4)
        .and_then(|split| filename.get(split..))
        .is_some_and(|extension| extension.eq_ignore_ascii_case(".3mf"))
}

/// The text the quantity regex's first group captures from `filename`, as
/// Node's `parseQuantity` reads it. The regex reads the name as written, so
/// saved regexes keep their meaning. Most end in `\.stl$`, so a `.3mf` name
/// they miss is read again as the `.stl` of the same name. `None` when
/// nothing matched or the group is empty.
#[cfg(feature = "desktop_rebuild")]
pub(crate) fn quantity_capture(regex: &regress::Regex, filename: &str) -> Option<String> {
    let capture = |name: &str| {
        regex.find(name).map(|m| {
            m.captures
                .first()
                .cloned()
                .flatten()
                .map(|r| name[r].to_owned())
        })
    };
    let captured = match capture(filename) {
        Some(group) => group,
        None if is_3mf(filename) => {
            capture(&format!("{}.stl", &filename[..filename.len() - 4])).flatten()
        }
        None => None,
    };
    captured.filter(|s| !s.is_empty())
}

/// Whether `marker` starts and ends with a symbol, so `strip_markers` removes
/// it anywhere in the name rather than only as a prefix.
#[cfg(feature = "desktop_rebuild")]
fn strips_anywhere(marker: &str) -> bool {
    let word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
    !word(marker.chars().next()) && !word(marker.chars().next_back())
}

/// `c` in lower case when that is one character, else `c`. Node's
/// `foldCharacter` does the same, so both compare markers alike.
#[cfg(feature = "desktop_rebuild")]
fn fold(c: char) -> char {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(folded), None) => folded,
        _ => c,
    }
}

/// Removes role markers from a part slug, as Node's `stripMarkers` does.
///
/// A marker that starts and ends with a symbol, like `[a]`, cannot be part of
/// a word, so it goes anywhere in the name. One left-to-right pass copies the
/// name and drops a marker, ignoring case, as soon as the copy ends with it.
/// A marker that removing a nested one exposes, as in `Panel_[a[c]]_Clip`,
/// goes too. Each removal takes one separator (space, `_` or `-`) on each
/// side, and puts one back when text remains on both sides: `Duct_[c]_Left`
/// becomes `Duct_Left`. A marker that starts or ends with a letter or digit,
/// like a custom `A`, then goes once as a prefix, so `gear` keeps its `a`.
///
/// Each removal shortens the copy or consumes input, so the work is linear in
/// the name's length times the markers' total length.
#[cfg(feature = "desktop_rebuild")]
pub(crate) fn strip_markers(stem: &str, markers: &[&str]) -> String {
    let separator = |c: &char| matches!(c, ' ' | '_' | '-');
    let folded: Vec<(bool, Vec<char>)> = markers
        .iter()
        .filter(|marker| !marker.is_empty())
        .map(|marker| (strips_anywhere(marker), marker.chars().map(fold).collect()))
        .collect();
    let input: Vec<char> = stem.chars().collect();
    let mut out: Vec<char> = Vec::with_capacity(input.len());
    let mut out_folded: Vec<char> = Vec::with_capacity(input.len());
    let mut next = 0;
    while let Some(&c) = input.get(next) {
        next += 1;
        out.push(c);
        out_folded.push(fold(c));
        while let Some((_, marker)) = folded
            .iter()
            .find(|(anywhere, marker)| *anywhere && out_folded.ends_with(marker))
        {
            let start = out.len() - marker.len();
            out.truncate(start);
            out_folded.truncate(start);
            let before = out.last().copied().filter(separator);
            if before.is_some() {
                out.pop();
                out_folded.pop();
            }
            let after = input.get(next).copied().filter(separator);
            if after.is_some() {
                next += 1;
            }
            if let Some(keep) = before.or(after)
                && !out.is_empty()
                && next < input.len()
            {
                out.push(keep);
                out_folded.push(fold(keep));
            }
        }
    }
    let mut rest = out.as_slice();
    for (_, marker) in folded.iter().filter(|(anywhere, _)| !anywhere) {
        if rest.len() >= marker.len() && rest.iter().zip(marker).all(|(&c, &m)| fold(c) == m) {
            rest = &rest[marker.len()..];
            if rest.first().is_some_and(separator) {
                rest = &rest[1..];
            }
        }
    }
    rest.iter().collect()
}

#[cfg(feature = "desktop_rebuild")]
mod preview;
#[cfg(feature = "desktop_rebuild")]
pub(super) use preview::preview;
#[cfg(feature = "desktop_rebuild")]
pub use preview::{NamingOverride, NamingPreview, QuantityOverride, RoleOverride, SlugOverride};
