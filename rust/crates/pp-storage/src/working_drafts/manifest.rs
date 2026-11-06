use super::{
    observation::{DocumentRead, PreparationBudget, ReadFailure, ReadResult},
    yaml::{Document, Node, number_text},
};
use crate::manifest_text::{
    DraftPart, JsText, ManifestText, OptionGroupId, VariantId, is_ecmascript_whitespace,
};

#[cfg(test)]
#[path = "manifest_glob_tests.rs"]
mod glob_tests;

#[derive(Clone, Default)]
pub(crate) struct Manifest {
    pub project: Option<ManifestText>,
    pub rules: Vec<PartRule>,
    pub groups: Groups,
    pub selections: Selections,
    pub selection_shapes: Vec<(OptionGroupId, bool)>,
}
#[derive(Clone)]
pub(crate) struct PartRule {
    pub pattern: ManifestText,
    pub requirement: Option<ManifestText>,
    pub group: Option<OptionGroupId>,
}
#[derive(Clone)]
pub(crate) struct Variant {
    pub id: VariantId,
    pub label: Option<ManifestText>,
    pub parts: Vec<ManifestText>,
    pub excludes: Vec<ManifestText>,
}
#[derive(Clone)]
pub(crate) struct Group {
    pub rule: String,
    pub label: Option<ManifestText>,
    pub parts: Vec<ManifestText>,
    pub variants: Vec<Variant>,
    pub min: Option<u64>,
    pub max: Option<u64>,
}
pub(crate) type Groups = Vec<(OptionGroupId, Group)>;
pub(crate) type Selections = Vec<(OptionGroupId, Vec<VariantId>)>;
fn invalid<T>() -> ReadResult<T> {
    Err(ReadFailure::InvalidDocument)
}
fn mapping(
    d: &Document,
    id: usize,
    b: &mut PreparationBudget<'_>,
) -> ReadResult<Vec<(JsText, usize)>> {
    match d.node(id, b)? {
        Node::Mapping(v) => {
            b.expansion(v.iter().map(|(key, _)| key.units().len() * 2).sum())?;
            Ok(v.clone())
        }
        _ => invalid(),
    }
}
fn sequence(d: &Document, id: usize, b: &mut PreparationBudget<'_>) -> ReadResult<Vec<usize>> {
    match d.node(id, b)? {
        Node::Sequence(v) => Ok(v.clone()),
        _ => invalid(),
    }
}
fn strings(
    d: &Document,
    id: Option<usize>,
    b: &mut PreparationBudget<'_>,
) -> ReadResult<Vec<ManifestText>> {
    let Some(id) = id else { return Ok(Vec::new()) };
    let Node::Sequence(v) = d.node(id, b)? else {
        return Ok(Vec::new());
    };
    v.iter().map(|i| d.text(*i, b).map(ManifestText)).collect()
}
fn bound(
    d: &Document,
    id: Option<usize>,
    b: &mut PreparationBudget<'_>,
) -> ReadResult<Option<u64>> {
    let Some(id) = id else { return Ok(None) };
    match d.node(id, b)? {
        Node::Null => Ok(None),
        Node::Number(n)
            if n.is_finite() && *n >= 0.0 && *n <= 9007199254740991.0 && n.fract() == 0.0 =>
        {
            Ok(Some(*n as u64))
        }
        _ => invalid(),
    }
}
fn selections(
    d: &Document,
    id: usize,
    b: &mut PreparationBudget<'_>,
) -> ReadResult<(Selections, Vec<(OptionGroupId, bool)>)> {
    let mut out = Vec::new();
    let mut shapes = Vec::new();
    for (key, id) in mapping(d, id, b)? {
        if key.is_ecmascript_blank() {
            return invalid();
        }
        let (ids, scalar) = match d.node(id, b)? {
            Node::Sequence(v) => (v.clone(), false),
            _ => (vec![id], true),
        };
        let mut values = Vec::new();
        for id in ids {
            let Node::String(s) = d.node(id, b)? else {
                return invalid();
            };
            b.expansion(s.units().len() * 2)?;
            let s = s.clone().trim_ecmascript();
            if s.is_empty() || values.iter().any(|v: &VariantId| v.0 == s) {
                return invalid();
            }
            values.push(VariantId(s));
        }
        b.expansion(key.units().len() * 2)?;
        shapes.push((OptionGroupId(key.clone()), scalar));
        out.push((OptionGroupId(key), values));
    }
    Ok((out, shapes))
}
fn groups(d: &Document, id: usize, b: &mut PreparationBudget<'_>) -> ReadResult<Groups> {
    let mut out = Vec::new();
    for (key, row) in mapping(d, id, b)? {
        b.rule()?;
        mapping(d, row, b)?;
        let min = bound(d, d.get(row, "min", b)?, b)?;
        let max = bound(d, d.get(row, "max", b)?, b)?;
        let rule = if let Some(id) = d.get(row, "rule", b)? {
            match d.node(id, b)? {
                Node::Null => "pick_one".into(),
                Node::String(s) => s
                    .as_scalar()
                    .filter(|rule| matches!(rule.as_str(), "pick_one" | "pick_any" | "pick_n"))
                    .ok_or(ReadFailure::InvalidDocument)?,
                _ => return invalid(),
            }
        } else {
            "pick_one".into()
        };
        if min.zip(max).is_some_and(|(a, z)| a > z)
            || (rule == "pick_one" && (min.unwrap_or(0) > 1 || max.unwrap_or(1) > 1))
        {
            return invalid();
        }
        let mut variants = Vec::new();
        if let Some(id) = d.get(row, "variants", b)?
            && let Node::Sequence(v) = d.node(id, b)?
        {
            for id in v {
                if !matches!(d.node(*id, b)?, Node::Mapping(_) | Node::Sequence(_)) {
                    continue;
                }
                let name = d
                    .optional_text(*id, "id", b)?
                    .unwrap_or_else(|| JsText::scalar(""))
                    .trim_ecmascript();
                if name.is_empty() {
                    continue;
                }
                b.rule()?;
                variants.push(Variant {
                    id: VariantId(name),
                    label: d.optional_text(*id, "label", b)?.map(ManifestText),
                    parts: strings(d, d.get(*id, "parts", b)?, b)?,
                    excludes: strings(d, d.get(*id, "excludes", b)?, b)?,
                });
            }
        }
        out.push((
            OptionGroupId(key),
            Group {
                rule,
                label: d.optional_text(row, "label", b)?.map(ManifestText),
                parts: strings(d, d.get(row, "parts", b)?, b)?,
                variants,
                min,
                max,
            },
        ));
    }
    Ok(out)
}
fn rules(
    d: &Document,
    id: Option<usize>,
    b: &mut PreparationBudget<'_>,
) -> ReadResult<Vec<PartRule>> {
    let Some(id) = id else { return Ok(Vec::new()) };
    let Node::Sequence(v) = d.node(id, b)? else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for id in v {
        match d.node(*id, b)? {
            Node::String(_) => {
                b.rule()?;
                out.push(PartRule {
                    pattern: ManifestText(d.text(*id, b)?),
                    requirement: None,
                    group: None,
                });
            }
            Node::Mapping(_) => {
                if let Some(m) = d.get(*id, "match", b)? {
                    b.rule()?;
                    if let Some(v) = d.get(*id, "default_included", b)? {
                        d.truthy(v, b)?;
                    }
                    out.push(PartRule {
                        pattern: ManifestText(d.text(m, b)?),
                        requirement: d.optional_text(*id, "requirement", b)?.map(ManifestText),
                        group: d.optional_text(*id, "option_group", b)?.map(OptionGroupId),
                    });
                }
            }
            _ => {}
        }
    }
    Ok(out)
}
fn blank(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).is_ok_and(|text| text.encode_utf16().all(is_ecmascript_whitespace))
}
pub(crate) fn parse(bytes: &[u8], b: &mut PreparationBudget<'_>) -> ReadResult<Manifest> {
    b.document(bytes.len(), 0)?;
    if blank(bytes) {
        return Ok(Manifest::default());
    }
    let d = Document::parse(bytes, b)?;
    mapping(&d, d.root, b)?;
    let (selections, selection_shapes) = if let Some(id) = d.get(d.root, "selections", b)? {
        selections(&d, id, b)?
    } else {
        (Vec::new(), Vec::new())
    };
    let mut out = Manifest {
        project: d.optional_text(d.root, "project", b)?.map(ManifestText),
        rules: rules(&d, d.get(d.root, "parts", b)?, b)?,
        groups: if let Some(id) = d.get(d.root, "option_groups", b)? {
            groups(&d, id, b)?
        } else {
            Vec::new()
        },
        selections,
        selection_shapes,
    };
    if let Some(id) = d.get(d.root, "addons", b)?
        && let Node::Sequence(v) = d.node(id, b)?
    {
        for a in v {
            if matches!(d.node(*a, b)?, Node::Null) {
                return invalid();
            }
            d.optional_text(*a, "project", b)?;
            d.optional_text(*a, "source_id", b)?;
            out.rules.extend(rules(&d, d.get(*a, "parts", b)?, b)?);
        }
    }
    for (key, ids) in &out.selections {
        if ids.is_empty() {
            return invalid();
        }
        if let Some((_, g)) = out.groups.iter().find(|(k, _)| k == key) {
            let max = if g.rule == "pick_one" {
                Some(g.max.unwrap_or(1).min(1))
            } else {
                g.max
            };
            if max.is_some_and(|n| ids.len() as u64 > n) {
                return invalid();
            }
        }
    }
    Ok(out)
}
/// One option of a variant dimension: a YAML string or number.
#[derive(Clone)]
pub(crate) enum DimensionOption {
    Text(JsText),
    Number(f64),
}
impl DimensionOption {
    /// `JSON.stringify` of the option; a non-finite number is `null`.
    pub(crate) fn write_json(&self, output: &mut Vec<u8>) {
        match self {
            Self::Text(text) => text.write_json(output),
            Self::Number(n) if n.is_finite() => {
                output.extend_from_slice(number_text(*n).as_bytes())
            }
            Self::Number(_) => output.extend_from_slice(b"null"),
        }
    }
    /// Node's `String(option) === value`.
    pub(crate) fn matches(&self, value: &str) -> bool {
        match self {
            Self::Text(text) => text.units().iter().copied().eq(value.encode_utf16()),
            Self::Number(n) => number_text(*n) == value,
        }
    }
}
pub(crate) type Dimensions = Vec<(JsText, Vec<DimensionOption>)>;
/// Node's `loadManifestYaml(text).variant_dimensions`: nothing unless the
/// whole manifest loads, then each entry of a mapping (or each index of a
/// sequence) whose value is a sequence, keeping its strings and numbers.
pub(crate) fn variant_dimensions(
    bytes: &[u8],
    b: &mut PreparationBudget<'_>,
) -> ReadResult<Dimensions> {
    parse(bytes, b)?;
    if blank(bytes) {
        return Ok(Vec::new());
    }
    let d = Document::parse(bytes, b)?;
    let Some(id) = d.get(d.root, "variant_dimensions", b)? else {
        return Ok(Vec::new());
    };
    let entries = match d.node(id, b)? {
        Node::Mapping(entries) => {
            b.expansion(entries.iter().map(|(key, _)| key.units().len() * 2).sum())?;
            entries.clone()
        }
        Node::Sequence(items) => items
            .iter()
            .enumerate()
            .map(|(index, item)| (JsText::scalar(&index.to_string()), *item))
            .collect(),
        _ => return Ok(Vec::new()),
    };
    let mut out = Vec::new();
    for (key, value) in entries {
        let Node::Sequence(items) = d.node(value, b)? else {
            continue;
        };
        let mut options = Vec::new();
        for item in items {
            match d.node(*item, b)? {
                Node::String(text) => {
                    b.expansion(text.units().len() * 2)?;
                    options.push(DimensionOption::Text(text.clone()));
                }
                Node::Number(n) => options.push(DimensionOption::Number(*n)),
                _ => {}
            }
        }
        out.push((key, options));
    }
    Ok(out)
}
pub(super) fn optional_document(
    read: &DocumentRead,
    b: &mut PreparationBudget<'_>,
) -> ReadResult<Option<Manifest>> {
    b.check()?;
    match read {
        DocumentRead::Bytes(bytes) => match parse(bytes, b) {
            Ok(d) => Ok(Some(d)),
            Err(ReadFailure::InvalidDocument) => b.check().map(|()| None),
            Err(e) => Err(e),
        },
        _ => Ok(None),
    }
}
fn union<E>(
    left: &mut Vec<ManifestText>,
    right: &[ManifestText],
    charge: &mut impl FnMut(usize) -> Result<(), E>,
) -> Result<(), E> {
    for s in std::mem::take(left) {
        let found = left.iter().position(|existing| *existing == s);
        charge(found.map_or(left.len(), |index| index + 1))?;
        if found.is_none() {
            left.push(s);
        }
    }
    for s in right {
        let found = left.iter().position(|existing| existing == s);
        charge(found.map_or(left.len(), |index| index + 1))?;
        if found.is_none() {
            left.push(s.clone());
        }
    }
    Ok(())
}

#[cfg(test)]
mod inspected_merge_tests {
    use super::super::observation::PreparationLimits;
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn group() -> Group {
        Group {
            rule: "pick_one".into(),
            label: None,
            parts: vec![],
            variants: vec![],
            min: None,
            max: None,
        }
    }

    #[test]
    fn early_group_matches_fit_inspected_work_grant() {
        let mut target: Groups = (0..128)
            .map(|i| {
                (
                    OptionGroupId(JsText::scalar(&format!("group-{i}"))),
                    group(),
                )
            })
            .collect();
        let incoming = (0..4).map(|_| (target[0].0.clone(), group())).collect();
        let cancelled = AtomicBool::new(false);
        let mut budget = PreparationBudget::new(
            &cancelled,
            PreparationLimits {
                comparison_work: 4 + 128,
                visits: 5,
                ..PreparationLimits::default()
            },
        )
        .unwrap();
        merge(&mut target, incoming, &mut budget).unwrap();
        assert_eq!(target.len(), 128);
        assert_eq!(target[0].0.0.as_scalar().as_deref(), Some("group-0"));
    }

    fn text(value: &str) -> ManifestText {
        ManifestText(JsText::scalar(value))
    }

    fn variant(id: &str) -> Variant {
        Variant {
            id: VariantId(JsText::scalar(id)),
            label: None,
            parts: vec![],
            excludes: vec![],
        }
    }

    fn named_group(id: &str, group: Group) -> (OptionGroupId, Group) {
        (OptionGroupId(JsText::scalar(id)), group)
    }

    fn assert_grants(target: Groups, incoming: Groups, expected: &[usize]) -> Groups {
        use super::super::observation::Resource;

        let mut actual = Vec::new();
        let mut result = target.clone();
        merge_with(&mut result, incoming.clone(), &mut |work| {
            actual.push(work);
            Ok::<_, ReadFailure>(())
        })
        .unwrap();
        assert_eq!(actual, expected);

        let work: usize = expected.iter().sum();
        for (comparison_work, visits, refusal) in [
            (work, expected.len(), None),
            (work, expected.len() - 1, Some(Resource::Visits)),
        ]
        .into_iter()
        .chain(
            work.checked_sub(1)
                .map(|under| (under, expected.len(), Some(Resource::ComparisonWork))),
        ) {
            let cancelled = AtomicBool::new(false);
            let mut budget = PreparationBudget::new(
                &cancelled,
                PreparationLimits {
                    comparison_work,
                    visits,
                    ..PreparationLimits::default()
                },
            )
            .unwrap();
            let outcome = merge(&mut target.clone(), incoming.clone(), &mut budget);
            match refusal {
                None => outcome.unwrap(),
                Some(resource) => {
                    let Err(ReadFailure::LimitExceeded(actual)) = outcome else {
                        panic!("expected {resource:?}, got {outcome:?}");
                    };
                    assert_eq!(
                        std::mem::discriminant(&actual),
                        std::mem::discriminant(&resource)
                    );
                }
            }
        }

        for stop in 0..expected.len() {
            let mut calls = Vec::new();
            let outcome = merge_with(&mut target.clone(), incoming.clone(), &mut |work| {
                calls.push(work);
                if calls.len() == stop + 1 {
                    Err(ReadFailure::Cancelled)
                } else {
                    Ok(())
                }
            });
            assert!(matches!(outcome, Err(ReadFailure::Cancelled)));
            assert_eq!(calls, expected[..=stop]);
        }

        // Each lookup checks cancellation in `visit` and again in `comparison`.
        for checks in 0..=expected.len() * 2 {
            let cancelled = AtomicBool::new(false);
            let mut budget =
                PreparationBudget::new(&cancelled, PreparationLimits::default()).unwrap();
            budget.cancel_after_checks(checks);
            let outcome = merge(&mut target.clone(), incoming.clone(), &mut budget);
            if checks < expected.len() * 2 {
                assert!(
                    matches!(outcome, Err(ReadFailure::Cancelled)),
                    "cancel after {checks} checks: {outcome:?}"
                );
            } else {
                outcome.unwrap();
            }
        }
        result
    }

    #[test]
    fn group_lookup_boundaries_and_sort_order() {
        let target = vec![
            named_group("2", group()),
            named_group("word", group()),
            named_group("1", group()),
        ];
        for (key, charges, ids) in [
            ("2", vec![1, 3], vec!["1", "2", "word"]),
            ("1", vec![3, 3], vec!["1", "2", "word"]),
            ("new", vec![3, 4], vec!["1", "2", "word", "new"]),
        ] {
            let result = assert_grants(target.clone(), vec![named_group(key, group())], &charges);
            assert_eq!(
                result
                    .iter()
                    .map(|(key, _)| key.0.as_scalar().unwrap())
                    .collect::<Vec<_>>(),
                ids
            );
        }
        let result = assert_grants(target, vec![], &[3]);
        assert_eq!(
            result
                .iter()
                .map(|(key, _)| key.0.as_scalar().unwrap())
                .collect::<Vec<_>>(),
            ["1", "2", "word"]
        );
        let result = assert_grants(vec![], vec![named_group("new", group())], &[0, 1]);
        assert_eq!(result[0].0.0.as_scalar().as_deref(), Some("new"));
        assert!(assert_grants(vec![], vec![], &[0]).is_empty());
    }

    #[test]
    fn variant_lookup_boundaries() {
        for (id, inspected) in [("a", 1), ("c", 3), ("d", 3)] {
            let mut prior = group();
            prior.variants = vec![variant("a"), variant("b"), variant("c")];
            let mut incoming = group();
            let mut v = variant(id);
            v.label = Some(text("incoming"));
            incoming.variants.push(v);
            let result = assert_grants(
                vec![named_group("g", prior)],
                vec![named_group("g", incoming)],
                &[1, 0, 1, 2, inspected, 1],
            );
            let variants = &result[0].1.variants;
            assert_eq!(
                variants
                    .iter()
                    .map(|v| v.id.0.as_scalar().unwrap())
                    .collect::<Vec<_>>(),
                if id == "d" {
                    vec!["a", "b", "c", "d"]
                } else {
                    vec!["a", "b", "c"]
                }
            );
            assert_eq!(
                variants
                    .iter()
                    .find(|v| v.id.0.as_scalar().as_deref() == Some(id))
                    .unwrap()
                    .label,
                Some(text("incoming"))
            );
        }
    }

    #[test]
    fn prior_variant_dedup_counts_matches_and_keeps_last_value_at_first_position() {
        for (id, inspected) in [("a", 1), ("c", 3), ("d", 3)] {
            let mut prior = group();
            prior.variants = vec![variant("a"), variant("b"), variant("c")];
            let mut last = variant(id);
            last.label = Some(text("last"));
            prior.variants.push(last);
            let result = assert_grants(
                vec![named_group("g", prior)],
                vec![named_group("g", group())],
                &[1, 0, 1, 2, inspected, 1],
            );
            let variants = &result[0].1.variants;
            assert_eq!(
                variants
                    .iter()
                    .map(|v| v.id.0.as_scalar().unwrap())
                    .collect::<Vec<_>>(),
                if id == "d" {
                    vec!["a", "b", "c", "d"]
                } else {
                    vec!["a", "b", "c"]
                }
            );
            assert_eq!(
                variants
                    .iter()
                    .find(|v| v.id.0.as_scalar().as_deref() == Some(id))
                    .unwrap()
                    .label,
                Some(text("last"))
            );
        }
    }

    #[test]
    fn path_lookup_boundaries_preserve_union_order() {
        for (path, inspected) in [("a", 1), ("c", 3), ("d", 3)] {
            let mut prior = group();
            prior.parts = vec![text("a"), text("b"), text("c")];
            let mut incoming = group();
            incoming.parts.push(text(path));
            let result = assert_grants(
                vec![named_group("g", prior)],
                vec![named_group("g", incoming)],
                &[1, 0, 1, 2, inspected, 1],
            );
            assert_eq!(
                result[0].1.parts,
                if path == "d" {
                    vec![text("a"), text("b"), text("c"), text("d")]
                } else {
                    vec![text("a"), text("b"), text("c")]
                }
            );
        }
    }

    #[test]
    fn merge_preserves_node_precedence_and_variant_path_dedup() {
        let mut prior = group();
        prior.rule = "pick_any".into();
        prior.label = Some(text("prior group"));
        prior.min = Some(1);
        let mut a = variant("a");
        a.label = Some(text("prior variant"));
        a.parts = vec![text("p"), text("p")];
        a.excludes = vec![text("x"), text("x")];
        prior.variants.push(a);
        let mut incoming = group();
        incoming.label = Some(text("incoming group"));
        incoming.min = Some(2);
        incoming.max = Some(3);
        let mut a = variant("a");
        a.label = Some(text("incoming variant"));
        a.parts = vec![text("p"), text("q")];
        a.excludes = vec![text("x"), text("y")];
        incoming.variants = vec![a, variant("b")];
        let result = assert_grants(
            vec![named_group("g", prior)],
            vec![named_group("g", incoming)],
            &[1, 0, 1, 0, 1, 1, 1, 0, 1, 1, 1, 1, 1],
        );
        let prior = &result[0].1;
        assert_eq!(prior.rule, "pick_any");
        assert_eq!(prior.label, Some(text("prior group")));
        assert_eq!((prior.min, prior.max), (Some(1), Some(3)));
        assert_eq!(prior.variants[0].label, Some(text("prior variant")));
        assert_eq!(prior.variants[0].parts, vec![text("p"), text("q")]);
        assert_eq!(prior.variants[0].excludes, vec![text("x"), text("y")]);
        assert_eq!(prior.variants[1].id.0.as_scalar().as_deref(), Some("b"));
    }
}
/// Merges option groups as Node does. Preparation charges a visit for each group,
/// variant, and path it merges and for the final sort, plus the entries each one
/// is compared against.
pub(crate) fn merge(
    target: &mut Groups,
    incoming: Groups,
    b: &mut PreparationBudget<'_>,
) -> ReadResult<()> {
    merge_with(target, incoming, &mut |compared| {
        b.visit(0)?;
        b.comparison(compared)
    })
}
/// Copies a document's groups, charging a visit for each group, variant, and path
/// before it is copied.
fn charged_copy(groups: &Groups, b: &mut PreparationBudget<'_>) -> ReadResult<Groups> {
    fn paths(
        list: &[ManifestText],
        b: &mut PreparationBudget<'_>,
    ) -> ReadResult<Vec<ManifestText>> {
        list.iter()
            .map(|path| b.visit(0).map(|()| path.clone()))
            .collect()
    }
    groups
        .iter()
        .map(|(key, g)| {
            b.visit(0)?;
            let variants = g
                .variants
                .iter()
                .map(|v| {
                    b.visit(0)?;
                    Ok(Variant {
                        id: v.id.clone(),
                        label: v.label.clone(),
                        parts: paths(&v.parts, b)?,
                        excludes: paths(&v.excludes, b)?,
                    })
                })
                .collect::<ReadResult<_>>()?;
            Ok((
                key.clone(),
                Group {
                    rule: g.rule.clone(),
                    label: g.label.clone(),
                    parts: paths(&g.parts, b)?,
                    variants,
                    min: g.min,
                    max: g.max,
                },
            ))
        })
        .collect()
}
pub(crate) fn merge_with<E>(
    target: &mut Groups,
    incoming: Groups,
    charge: &mut impl FnMut(usize) -> Result<(), E>,
) -> Result<(), E> {
    for (key, g) in incoming {
        let found = target.iter().position(|(k, _)| *k == key);
        charge(found.map_or(target.len(), |index| index + 1))?;
        if let Some(index) = found {
            let prior = &mut target[index].1;
            let mut variants: Vec<Variant> = Vec::new();
            for variant in std::mem::take(&mut prior.variants) {
                let found = variants
                    .iter()
                    .position(|existing| existing.id == variant.id);
                charge(found.map_or(variants.len(), |index| index + 1))?;
                if let Some(index) = found {
                    variants[index] = variant;
                } else {
                    variants.push(variant);
                }
            }
            prior.variants = variants;
            union(&mut prior.parts, &g.parts, charge)?;
            for v in g.variants {
                let found = prior.variants.iter().position(|p| p.id == v.id);
                charge(found.map_or(prior.variants.len(), |index| index + 1))?;
                if let Some(index) = found {
                    let p = &mut prior.variants[index];
                    union(&mut p.parts, &v.parts, charge)?;
                    union(&mut p.excludes, &v.excludes, charge)?;
                    if p.label.as_ref().is_none_or(|s| s.is_empty())
                        && v.label.as_ref().is_some_and(|s| !s.is_empty())
                    {
                        p.label = v.label
                    }
                } else {
                    prior.variants.push(v)
                }
            }
            if prior.label.as_ref().is_none_or(|s| s.is_empty()) {
                prior.label = g.label
            }
            prior.min = prior.min.or(g.min);
            prior.max = prior.max.or(g.max);
        } else {
            target.push((key, g));
        }
    }
    charge(target.len())?;
    target.sort_by(
        |(a, _), (b, _)| match (a.0.array_index(), b.0.array_index()) {
            (Some(a), Some(b)) => a.cmp(&b),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            _ => std::cmp::Ordering::Equal,
        },
    );
    Ok(())
}
fn normalize_glob(text: &JsText) -> JsText {
    let mut units = Vec::with_capacity(text.units().len());
    let mut scalar = String::new();
    for decoded in char::decode_utf16(text.units().iter().copied()) {
        match decoded {
            Ok(c) => scalar.push(if c == '\\' { '/' } else { c }),
            Err(error) => {
                units.extend(scalar.to_lowercase().encode_utf16());
                scalar.clear();
                units.push(error.unpaired_surrogate());
            }
        }
    }
    units.extend(scalar.to_lowercase().encode_utf16());
    JsText::from_units(units).trim_ecmascript()
}

fn canonicalize_regex_unit(unit: u16) -> u16 {
    let Some(c) = char::from_u32(u32::from(unit)) else {
        return unit;
    };
    let mut uppercase = c.to_uppercase();
    let Some(upper) = uppercase.next() else {
        return unit;
    };
    if uppercase.next().is_some() || upper.len_utf16() != 1 {
        return unit;
    }
    let upper = upper as u16;
    if unit >= 0x80 && upper < 0x80 {
        unit
    } else {
        upper
    }
}

fn dot_matches(unit: u16) -> bool {
    !matches!(unit, 0x000a | 0x000d | 0x2028 | 0x2029)
}

fn unit_matches(pattern: u16, key: u16) -> bool {
    if pattern == 0x3f {
        dot_matches(key)
    } else {
        pattern == key
    }
}

fn fits(segment: &[u16], key: &[u16], work: &mut usize) -> bool {
    *work += segment.len();
    segment.len() == key.len() && segment.iter().zip(key).all(|(p, k)| unit_matches(*p, *k))
}

/// Matches `^pattern$` with `*` as `.*` and `?` as `.`. The work is
/// O(key + pattern) unless a segment between stars is longer than 64 units and
/// contains `?`; such a segment costs O((key + segment) log segment).
///
/// Placing each middle segment at its leftmost reachable start is optimal: a
/// `*` cannot cross a line terminator, and two starts before the same
/// terminator can only both match if neither covers it.
fn wildcard_matches(pattern: &[u16], key: &[u16], work: &mut usize) -> bool {
    let mut segments: Vec<_> = pattern.split(|unit| *unit == 0x2a).collect();
    let tail = segments.pop().unwrap_or_default();
    if segments.is_empty() {
        return fits(tail, key, work);
    }
    let head = segments[0];
    let Some(end) = key.len().checked_sub(tail.len()) else {
        return false;
    };
    if head.len() > end || !fits(head, &key[..head.len()], work) || !fits(tail, &key[end..], work) {
        return false;
    }
    let mut cursor = head.len();
    let mut terminator = cursor;
    for segment in segments[1..].iter().filter(|segment| !segment.is_empty()) {
        if terminator < cursor {
            terminator = cursor;
        }
        while terminator < key.len() && dot_matches(key[terminator]) {
            *work += 1;
            terminator += 1;
        }
        let Some(last) = end.checked_sub(segment.len()) else {
            return false;
        };
        let last = last.min(terminator);
        if last < cursor {
            return false;
        }
        let window = &key[cursor..last + segment.len()];
        let found = if !segment.contains(&0x3f) {
            find_literal(segment, window, work)
        } else if segment.len() <= 64 {
            find_short_masked(segment, window, work)
        } else {
            find_masked(segment, window, work)
        };
        let Some(start) = found else {
            return false;
        };
        cursor += start + segment.len();
    }
    *work += end - cursor;
    key[cursor..end].iter().copied().all(dot_matches)
}

/// Knuth-Morris-Pratt search for a segment without `?`.
fn find_literal(needle: &[u16], haystack: &[u16], work: &mut usize) -> Option<usize> {
    let mut border = vec![0; needle.len()];
    let mut matched = 0;
    for index in 1..needle.len() {
        while matched > 0 && needle[index] != needle[matched] {
            *work += 1;
            matched = border[matched - 1];
        }
        *work += 1;
        if needle[index] == needle[matched] {
            matched += 1;
        }
        border[index] = matched;
    }
    matched = 0;
    for (index, unit) in haystack.iter().enumerate() {
        while matched > 0 && *unit != needle[matched] {
            *work += 1;
            matched = border[matched - 1];
        }
        *work += 1;
        if *unit == needle[matched] {
            matched += 1;
            if matched == needle.len() {
                return Some(index + 1 - matched);
            }
        }
    }
    None
}

/// Shift-And search for a segment of at most 64 units that contains `?`.
fn find_short_masked(needle: &[u16], haystack: &[u16], work: &mut usize) -> Option<usize> {
    let mut literals: Vec<(u16, u64)> = Vec::new();
    let mut any = 0;
    for (offset, unit) in needle.iter().enumerate() {
        if *unit == 0x3f {
            any |= 1 << offset;
        } else {
            match literals.binary_search_by_key(unit, |(literal, _)| *literal) {
                Ok(at) => literals[at].1 |= 1 << offset,
                Err(at) => literals.insert(at, (*unit, 1 << offset)),
            }
        }
    }
    let done = 1 << (needle.len() - 1);
    let mut state = 0_u64;
    for (index, unit) in haystack.iter().enumerate() {
        *work += 1;
        let literal = literals
            .binary_search_by_key(unit, |(literal, _)| *literal)
            .map_or(0, |at| literals[at].1);
        let allowed = if dot_matches(*unit) {
            literal | any
        } else {
            literal
        };
        state = (state << 1 | 1) & allowed;
        if state & done != 0 {
            return Some(index + 1 - needle.len());
        }
    }
    None
}

const NTT_PRIME: u64 = 29 << 57 | 1;
const NTT_ROOT: u64 = 3;
/// `-NTT_PRIME⁻¹ mod 2^64` by Newton iteration, for Montgomery reduction.
const NEG_INVERSE: u64 = {
    let mut inverse = 1_u64;
    let mut round = 0;
    while round < 6 {
        inverse = inverse.wrapping_mul(2_u64.wrapping_sub(NTT_PRIME.wrapping_mul(inverse)));
        round += 1;
    }
    inverse.wrapping_neg()
};
/// `2^128 mod NTT_PRIME`; multiplying by it moves a value into Montgomery form.
const MONTGOMERY_SQUARE: u64 = ((u128::MAX % NTT_PRIME as u128 + 1) % NTT_PRIME as u128) as u64;

/// `a · b · 2^-64 mod NTT_PRIME` for `a, b < NTT_PRIME`.
fn mont_mul(a: u64, b: u64) -> u64 {
    let product = u128::from(a) * u128::from(b);
    let m = (product as u64).wrapping_mul(NEG_INVERSE);
    let reduced = ((product + u128::from(m) * u128::from(NTT_PRIME)) >> 64) as u64;
    reduced.checked_sub(NTT_PRIME).unwrap_or(reduced)
}

fn mont(value: u64) -> u64 {
    mont_mul(value, MONTGOMERY_SQUARE)
}

fn add_mod(a: u64, b: u64) -> u64 {
    let sum = a + b;
    sum.checked_sub(NTT_PRIME).unwrap_or(sum)
}

fn pow_mod(base: u64, mut exponent: u64) -> u64 {
    let (mut base, mut result) = (mont(base), mont(1));
    while exponent > 0 {
        if exponent & 1 == 1 {
            result = mont_mul(result, base);
        }
        base = mont_mul(base, base);
        exponent >>= 1;
    }
    mont_mul(result, 1)
}

/// In-place number-theoretic transform of plain residues.
fn ntt(values: &mut [u64], inverse: bool, work: &mut usize) {
    let n = values.len();
    *work += n * n.trailing_zeros() as usize;
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            values.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let mut step = pow_mod(NTT_ROOT, (NTT_PRIME - 1) / len as u64);
        if inverse {
            step = pow_mod(step, NTT_PRIME - 2);
        }
        let step = mont(step);
        for chunk in values.chunks_mut(len) {
            let (low, high) = chunk.split_at_mut(len / 2);
            let mut twiddle = mont(1);
            for (a, b) in low.iter_mut().zip(high) {
                let t = mont_mul(*b, twiddle);
                *b = add_mod(*a, NTT_PRIME - t);
                *a = add_mod(*a, t);
                twiddle = mont_mul(twiddle, step);
            }
        }
        len <<= 1;
    }
    if inverse {
        let scale = mont(pow_mod(n as u64, NTT_PRIME - 2));
        values
            .iter_mut()
            .for_each(|value| *value = mont_mul(*value, scale));
    }
}

/// Finds the first start where every literal unit of `needle` equals the key
/// and every `?` covers a non-terminator, by testing
/// `Σ literal (p - k)² + Σ ? · terminator(k) = 0` with NTT correlations over
/// blocks of `2^⌈log₂ 2m⌉` units. Units are ranked within the needle (at most
/// 2^16 ranks), so each sum stays below `m · 2^32 + m`, far under the prime.
fn find_masked(needle: &[u16], haystack: &[u16], work: &mut usize) -> Option<usize> {
    let m = needle.len();
    let size = (2 * m).next_power_of_two();
    let mut ranks: Vec<u16> = needle
        .iter()
        .copied()
        .filter(|unit| *unit != 0x3f)
        .collect();
    ranks.sort_unstable();
    ranks.dedup();
    let rank = |unit: u16| {
        ranks
            .binary_search(&unit)
            .map_or(0, |index| index as u64 + 1)
    };
    let mut weighted = vec![0; size];
    let mut literal = vec![0; size];
    let mut any = vec![0; size];
    let mut constant = 0;
    for (offset, unit) in needle.iter().rev().enumerate() {
        if *unit == 0x3f {
            any[offset] = 1;
        } else {
            let r = rank(*unit);
            weighted[offset] = NTT_PRIME - 2 * r;
            literal[offset] = 1;
            constant += r * r;
        }
    }
    // Montgomery-form spectra make each pointwise product a plain residue.
    for pattern in [&mut weighted, &mut literal, &mut any] {
        ntt(pattern, false, work);
        pattern.iter_mut().for_each(|value| *value = mont(*value));
    }
    let constant = constant % NTT_PRIME;
    let (mut ranked, mut squared, mut terminators) = (vec![0; size], vec![0; size], vec![0; size]);
    let mut block = 0;
    while block + m <= haystack.len() {
        let text = &haystack[block..haystack.len().min(block + size)];
        for values in [&mut ranked, &mut squared, &mut terminators] {
            values.fill(0);
        }
        for (index, unit) in text.iter().enumerate() {
            let r = rank(*unit);
            ranked[index] = r;
            squared[index] = r * r;
            terminators[index] = u64::from(!dot_matches(*unit));
        }
        for values in [&mut ranked, &mut squared, &mut terminators] {
            ntt(values, false, work);
        }
        for i in 0..size {
            ranked[i] = add_mod(
                add_mod(
                    mont_mul(weighted[i], ranked[i]),
                    mont_mul(literal[i], squared[i]),
                ),
                mont_mul(any[i], terminators[i]),
            );
        }
        ntt(&mut ranked, true, work);
        *work += text.len() + 1 - m;
        if let Some(start) =
            (0..=text.len() - m).find(|start| add_mod(ranked[start + m - 1], constant) == 0)
        {
            return Some(block + start);
        }
        block += size + 1 - m;
    }
    None
}

pub(crate) struct Glob {
    text: JsText,
    units: Vec<u16>,
}

impl Glob {
    pub(crate) fn new(pattern: &ManifestText) -> Self {
        let text = normalize_glob(pattern);
        let units = text
            .units()
            .iter()
            .copied()
            .map(canonicalize_regex_unit)
            .collect();
        Self { text, units }
    }

    pub(crate) fn matches(&self, key: &str) -> bool {
        self.counted_matches(key).0
    }

    /// Counts normalized units plus matcher steps as the work done.
    fn counted_matches(&self, key: &str) -> (bool, usize) {
        let key = normalize_glob(&JsText::scalar(key));
        let mut work = self.text.units().len() + key.units().len();
        if self.text == key {
            return (true, work);
        }
        let key: Vec<_> = key
            .units()
            .iter()
            .copied()
            .map(canonicalize_regex_unit)
            .collect();
        let matched = wildcard_matches(&self.units, &key, &mut work)
            || (!self.units.contains(&0x2f)
                && key.contains(&0x2f)
                && key
                    .rsplit(|unit| *unit == 0x2f)
                    .next()
                    .is_some_and(|basename| wildcard_matches(&self.units, basename, &mut work)));
        (matched, work)
    }
}
pub(crate) fn hints(bytes: &[u8], b: &mut PreparationBudget<'_>) -> ReadResult<Vec<HintRule>> {
    b.document(bytes.len(), 0)?;
    let d = Document::parse(bytes, b)?;
    let root = mapping(&d, d.root, b)?;
    if root
        .iter()
        .any(|(k, _)| !matches!(k.as_scalar().as_deref(), Some("version" | "rules")))
    {
        return invalid();
    }
    let version = d
        .get(d.root, "version", b)?
        .ok_or(ReadFailure::InvalidDocument)?;
    if !matches!(d.node(version,b)?,Node::Number(n) if *n==1.0) {
        return invalid();
    }
    let rules = d
        .get(d.root, "rules", b)?
        .ok_or(ReadFailure::InvalidDocument)?;
    let mut out = Vec::new();
    for id in sequence(&d, rules, b)? {
        b.rule()?;
        let map = mapping(&d, id, b)?;
        if map.iter().any(|(k, _)| {
            !matches!(
                k.as_scalar().as_deref(),
                Some("path" | "option_group" | "variant_id" | "label")
            )
        }) {
            return invalid();
        }
        out.push((
            ManifestText(hint_field(&d, id, "path", b)?.ok_or(ReadFailure::InvalidDocument)?),
            OptionGroupId(
                hint_field(&d, id, "option_group", b)?.ok_or(ReadFailure::InvalidDocument)?,
            ),
            VariantId(hint_field(&d, id, "variant_id", b)?.ok_or(ReadFailure::InvalidDocument)?),
            hint_field(&d, id, "label", b)?.map(ManifestText),
        ));
    }
    Ok(out)
}

fn hint_field(
    d: &Document,
    row: usize,
    key: &str,
    b: &mut PreparationBudget<'_>,
) -> ReadResult<Option<JsText>> {
    let Some(id) = d.get(row, key, b)? else {
        return Ok(None);
    };
    match d.node(id, b)? {
        Node::String(s) if !s.is_ecmascript_blank() => {
            b.expansion(s.units().len() * 2)?;
            Ok(Some(s.clone().trim_ecmascript()))
        }
        _ => invalid(),
    }
}

pub(crate) type HintRule = (ManifestText, OptionGroupId, VariantId, Option<ManifestText>);

pub(crate) fn infer_siblings(
    paths: &[String],
    budget: &mut PreparationBudget<'_>,
) -> ReadResult<Groups> {
    super::siblings::infer(paths, budget)
}

fn any_path(
    patterns: &[ManifestText],
    key: &str,
    b: &mut PreparationBudget<'_>,
) -> ReadResult<bool> {
    for pattern in patterns {
        if path_matches(pattern, key, b)? {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn path_matches(
    pattern: &ManifestText,
    key: &str,
    b: &mut PreparationBudget<'_>,
) -> ReadResult<bool> {
    b.visit(0)?;
    // Normalization scans the raw text before trimming, so charge it up front.
    b.comparison(pattern.units().len().saturating_add(key.len()))?;
    let (matched, work) = Glob::new(pattern).counted_matches(key);
    b.comparison(work)?;
    Ok(matched)
}

pub(super) fn apply(
    parts: &mut [DraftPart],
    docs: &[(String, String, Manifest)],
    mut groups: Groups,
    mut selections: Selections,
    b: &mut PreparationBudget<'_>,
) -> ReadResult<()> {
    b.check()?;
    for (_, _, d) in docs {
        b.visit(0)?;
        for (key, s) in &d.selections {
            b.visit(0)?;
            if !selections.iter().any(|(k, _)| k == key) {
                b.expansion(
                    key.0
                        .units()
                        .len()
                        .saturating_add(s.iter().map(|id| id.0.units().len()).sum::<usize>())
                        .saturating_mul(2),
                )?;
                selections.push((key.clone(), s.clone()));
            }
        }
    }
    let inferred = std::mem::take(&mut groups);
    for (_, _, d) in docs {
        b.visit(0)?;
        let copy = charged_copy(&d.groups, b)?;
        merge(&mut groups, copy, b)?;
    }
    merge(&mut groups, inferred, b)?;
    for part in parts {
        let key = part["partKey"].as_str().unwrap_or_default();
        b.entry(0, key.len())?;
        let key = key.to_owned();
        let label = part["sourceLayer"]
            .as_str()
            .unwrap_or_default()
            .split(':')
            .nth(1);
        for (name, source, d) in docs {
            b.visit(0)?;
            if d.project.as_ref().is_some_and(|p| {
                !p.is_empty()
                    && label.is_some_and(|l| {
                        !l.is_empty() && p.as_scalar().as_deref() != Some(l) && name != l
                    })
            }) {
                continue;
            }
            let mut matched = None;
            for rule in &d.rules {
                if path_matches(&rule.pattern, &key, b)? {
                    matched = Some(rule);
                    break;
                }
            }
            if let Some(rule) = matched {
                if let Some(v) = &rule.requirement {
                    part.manifest.requirement = Some(v.clone());
                }
                if let Some(v) = &rule.group {
                    part.manifest.option_group_id = Some(v.clone());
                }
                part.manifest.manifest_source = Some(source.clone());
                break;
            }
        }
        let explicit = part.manifest.option_group_id.as_ref();
        let mut member = false;
        let mut included = false;
        let mut excluded = false;
        for (id, g) in &groups {
            b.visit(0)?;
            let mut ids = Vec::new();
            if let Some((_, values)) = selections.iter().find(|(k, _)| k == id) {
                for id in values {
                    b.visit(0)?;
                    if g.variants.is_empty() || g.variants.iter().any(|v| v.id == *id) {
                        ids.push(id);
                    }
                }
            }
            let max = if g.rule == "pick_one" {
                g.max.unwrap_or(1).min(1)
            } else {
                g.max.unwrap_or(u64::MAX)
            };
            let complete = (ids.len() as u64) >= g.min.unwrap_or(0) && (ids.len() as u64) <= max;
            let membership = if let Some(explicit) = explicit {
                explicit == id
            } else {
                let mut found = any_path(&g.parts, &key, b)?;
                for variant in &g.variants {
                    b.visit(0)?;
                    if found {
                        break;
                    }
                    found = any_path(&variant.parts, &key, b)?;
                }
                found
            };
            member |= membership;
            if complete {
                for selected in ids {
                    b.visit(0)?;
                    let variant = g.variants.iter().find(|v| v.id == *selected);
                    if membership {
                        included |= if let Some(v) = variant {
                            any_path(&v.parts, &key, b)?
                        } else {
                            path_matches(&ManifestText(selected.0.clone()), &key, b)?
                        };
                    }
                    if explicit.is_none_or(|v| v == id)
                        && let Some(v) = variant
                    {
                        excluded |= any_path(&v.excludes, &key, b)?;
                    }
                }
            }
        }
        if member {
            part["included"] = included.into();
        }
        if excluded {
            part["included"] = false.into();
        }
    }
    b.check()
}

pub(super) fn kit(raw: Option<String>, b: &mut PreparationBudget<'_>) -> ReadResult<Selections> {
    b.check()?;
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let selections =
        crate::manifest_options::observation::stored_kit(&raw, b)?.selection_projection();
    b.check()?;
    Ok(selections)
}

#[cfg(test)]
#[path = "manifest_preparation_tests.rs"]
mod preparation_tests;

#[cfg(test)]
mod variant_dimension_tests {
    use super::*;
    use crate::working_drafts::observation::{PreparationLimits, Resource};
    use std::sync::atomic::AtomicBool;

    #[test]
    fn aliased_variant_options_charge_their_copies() {
        let long = "x".repeat(1000);
        let aliases = vec!["*s"; 200].join(", ");
        let source = format!("variant_dimensions:\n  a: [&s {long}]\n  b: [{aliases}]\n");
        let cancel = AtomicBool::new(false);
        let mut budget = PreparationBudget::new(&cancel, PreparationLimits::default()).unwrap();
        let dimensions = variant_dimensions(source.as_bytes(), &mut budget).unwrap();
        assert_eq!(dimensions[1].1.len(), 200);
        let mut budget = PreparationBudget::new(
            &cancel,
            PreparationLimits {
                total_document_bytes: 20_000,
                ..PreparationLimits::default()
            },
        )
        .unwrap();
        assert!(matches!(
            variant_dimensions(source.as_bytes(), &mut budget),
            Err(ReadFailure::LimitExceeded(Resource::DocumentBytes))
        ));
    }
}
