use super::model::{
    AcceptedPlanBasis, Digest, DimensionUm, OffsetUm, PlateId, RequiredUnitToken,
    millimetres_to_micrometres, trim_ecmascript, trimmed_text,
};
use anyhow::Result;
use sha2::{Digest as _, Sha256};
use std::collections::{HashMap, HashSet};

pub const MAX_ACCEPTED_PLATES: usize = 65_534;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Placement {
    Auto,
    Manual,
    Unplaced,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnitInput {
    pub token: RequiredUnitToken,
    pub x_um: OffsetUm,
    pub y_um: OffsetUm,
    pub width_um: DimensionUm,
    pub depth_um: DimensionUm,
    pub height_um: DimensionUm,
    pub placement: Placement,
    pub pinned: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrinterGeometry {
    pub bed_width_um: DimensionUm,
    pub bed_depth_um: DimensionUm,
    pub bed_height_um: DimensionUm,
    pub margin_um: OffsetUm,
}

impl PrinterGeometry {
    pub fn from_millimetres(
        bed_width_mm: f64,
        bed_depth_mm: f64,
        bed_height_mm: f64,
        margin_mm: f64,
    ) -> Option<Self> {
        let bed_width_um = millimetres_to_micrometres(bed_width_mm)?;
        let bed_depth_um = millimetres_to_micrometres(bed_depth_mm)?;
        let bed_height_um = millimetres_to_micrometres(bed_height_mm)?;
        let margin_um = millimetres_to_micrometres(margin_mm)?;
        if margin_um < 0
            || margin_um.checked_mul(2)? >= bed_width_um
            || margin_um.checked_mul(2)? >= bed_depth_um
        {
            return None;
        }
        Some(Self {
            bed_width_um: DimensionUm::parse(bed_width_um).ok()?,
            bed_depth_um: DimensionUm::parse(bed_depth_um).ok()?,
            bed_height_um: DimensionUm::parse(bed_height_um).ok()?,
            margin_um: OffsetUm::parse(margin_um).ok()?,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlateInput {
    pub plate_id: PlateId,
    pub printer_id: String,
    pub printer_name: String,
    pub printer_model: String,
    pub printer: PrinterGeometry,
    pub units: Vec<UnitInput>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedPlate {
    pub ordinal: u32,
    pub input: PlateInput,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Spacing {
    Clearance,
    OverlapOnly,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayoutFailure {
    InvalidUnits,
    OutsideBuildArea,
    OverlappingUnits,
}

pub fn violates_clearance(left: &UnitInput, right: &UnitInput, clearance: u32) -> bool {
    let (lx, ly, lw, ld) = (
        u64::from(left.x_um.get()),
        u64::from(left.y_um.get()),
        u64::from(left.width_um.get()),
        u64::from(left.depth_um.get()),
    );
    let (rx, ry, rw, rd) = (
        u64::from(right.x_um.get()),
        u64::from(right.y_um.get()),
        u64::from(right.width_um.get()),
        u64::from(right.depth_um.get()),
    );
    lx < rx + rw + u64::from(clearance)
        && rx < lx + lw + u64::from(clearance)
        && ly < ry + rd + u64::from(clearance)
        && ry < ly + ld + u64::from(clearance)
}

pub fn validate_plates(
    mut plates: Vec<PlateInput>,
    expected: &HashSet<RequiredUnitToken>,
    require_every: bool,
    spacing: Spacing,
) -> std::result::Result<Vec<ValidatedPlate>, LayoutFailure> {
    if plates.is_empty() || plates.len() > MAX_ACCEPTED_PLATES || expected.is_empty() {
        return Err(LayoutFailure::InvalidUnits);
    }
    let mut plate_ids = HashSet::new();
    let mut tokens = HashSet::new();
    for plate in &mut plates {
        for text in [
            &mut plate.printer_id,
            &mut plate.printer_name,
            &mut plate.printer_model,
        ] {
            *text = trimmed_text(text).ok_or(LayoutFailure::InvalidUnits)?;
        }
        if !plate_ids.insert(plate.plate_id.clone()) {
            return Err(LayoutFailure::InvalidUnits);
        }
        let width = plate.printer.bed_width_um.get();
        let depth = plate.printer.bed_depth_um.get();
        let height = plate.printer.bed_height_um.get();
        let margin = plate.printer.margin_um.get();
        if u64::from(margin) * 2 > u64::from(width) || u64::from(margin) * 2 > u64::from(depth) {
            return Err(LayoutFailure::OutsideBuildArea);
        }
        for unit in &mut plate.units {
            if !expected.contains(&unit.token) || !tokens.insert(unit.token.clone()) {
                return Err(LayoutFailure::InvalidUnits);
            }
            if unit.placement == Placement::Unplaced {
                unit.pinned = false;
                continue;
            }
            let x = unit.x_um.get();
            let y = unit.y_um.get();
            if x < margin
                || y < margin
                || u64::from(x) + u64::from(unit.width_um.get()) > u64::from(width - margin)
                || u64::from(y) + u64::from(unit.depth_um.get()) > u64::from(depth - margin)
                || unit.height_um.get() > height
            {
                return Err(LayoutFailure::OutsideBuildArea);
            }
        }
        let placed: Vec<_> = plate
            .units
            .iter()
            .filter(|unit| unit.placement != Placement::Unplaced)
            .collect();
        for (index, left) in placed.iter().enumerate() {
            for right in &placed[index + 1..] {
                if violates_clearance(
                    left,
                    right,
                    if spacing == Spacing::Clearance {
                        margin
                    } else {
                        0
                    },
                ) {
                    return Err(LayoutFailure::OverlappingUnits);
                }
            }
        }
    }
    if tokens.is_empty() || (require_every && tokens.len() != expected.len()) {
        return Err(LayoutFailure::InvalidUnits);
    }
    Ok(plates
        .into_iter()
        .enumerate()
        .map(|(index, input)| ValidatedPlate {
            ordinal: (index + 1) as u32,
            input,
        })
        .collect())
}

fn quoted(value: &str) -> String {
    serde_json::Value::String(value.to_owned()).to_string()
}
fn placement(value: Placement) -> &'static str {
    match value {
        Placement::Auto => "auto",
        Placement::Manual => "manual",
        Placement::Unplaced => "unplaced",
    }
}

pub fn layout_digest(plates: &[ValidatedPlate], format: u8) -> Digest {
    let mut output = format!("{{\"format\":{format},\"plates\":[");
    for (plate_index, plate) in plates.iter().enumerate() {
        if plate_index > 0 {
            output.push(',');
        }
        let item = &plate.input;
        output.push_str(&format!("{{\"ordinal\":{},\"plateId\":{},\"printerId\":{},\"printerName\":{},\"printerModel\":{},\"bedWidthUm\":{},\"bedDepthUm\":{},\"bedHeightUm\":{},\"marginUm\":{},\"units\":[", plate.ordinal, quoted(item.plate_id.as_str()), quoted(&item.printer_id), quoted(&item.printer_name), quoted(&item.printer_model), item.printer.bed_width_um.get(), item.printer.bed_depth_um.get(), item.printer.bed_height_um.get(), item.printer.margin_um.get()));
        let mut units = item.units.iter().collect::<Vec<_>>();
        units.sort_by(|left, right| left.token.as_str().cmp(right.token.as_str()));
        for (unit_index, unit) in units.iter().enumerate() {
            if unit_index > 0 {
                output.push(',');
            }
            output.push_str(&format!("{{\"token\":{},\"xUm\":{},\"yUm\":{},\"widthUm\":{},\"depthUm\":{},\"heightUm\":{}", quoted(unit.token.as_str()), unit.x_um.get(), unit.y_um.get(), unit.width_um.get(), unit.depth_um.get(), unit.height_um.get()));
            if format >= 2 {
                output.push_str(&format!(
                    ",\"placement\":{},\"pinned\":{}",
                    quoted(placement(unit.placement)),
                    unit.pinned
                ));
            }
            output.push('}');
        }
        output.push_str("]}");
    }
    output.push_str("]}");
    Digest::from_bytes(Sha256::digest(output.as_bytes()).into())
}

pub fn initial_plate_id(
    basis: &AcceptedPlanBasis,
    printer_id: &str,
    tokens: &[RequiredUnitToken],
) -> Result<PlateId> {
    basis.validate()?;
    let mut names = tokens
        .iter()
        .map(|token| token.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    let input = serde_json::to_string(&(
        "accepted-plate-v1",
        basis.plan_revision_digest.as_str(),
        basis.required_unit_mapping_digest.as_str(),
        printer_id,
        names,
    ))?;
    PlateId::parse(format!(
        "plate_{}",
        &hex::encode(Sha256::digest(input.as_bytes()))[..32]
    ))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackingUnit {
    pub token: RequiredUnitToken,
    pub width_um: DimensionUm,
    pub depth_um: DimensionUm,
    pub height_um: DimensionUm,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackedUnit {
    pub unit: PackingUnit,
    pub x_um: OffsetUm,
    pub y_um: OffsetUm,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackResult {
    Packed(Vec<Vec<PackedUnit>>),
    UnitTooLarge(RequiredUnitToken),
}

fn printable_dimensions(printer: &PrinterGeometry) -> Option<(u32, u32)> {
    let double_margin = printer.margin_um.get().checked_mul(2)?;
    Some((
        printer.bed_width_um.get().checked_sub(double_margin)?,
        printer.bed_depth_um.get().checked_sub(double_margin)?,
    ))
}

fn fits(printer: &PrinterGeometry, unit: &PackingUnit) -> bool {
    let Some((usable_width, usable_depth)) = printable_dimensions(printer) else {
        return false;
    };
    unit.width_um.get() <= usable_width
        && unit.depth_um.get() <= usable_depth
        && unit.height_um.get() <= printer.bed_height_um.get()
}
/// A unit's token and its measured width, depth and height in micrometres.
pub type MeasuredUnit = (RequiredUnitToken, [u64; 3]);

/// The first unit, in packing order, that does not fit a bed given as width,
/// depth, height and margin in micrometres: Node's `unitFitsPrinter` over raw
/// measurements, before any value has to fit the stored Plate range.
pub fn first_too_large(bed_um: [i64; 4], units: &[MeasuredUnit]) -> Option<RequiredUnitToken> {
    let [width, depth, height, margin] = bed_um.map(i128::from);
    let mut units = units.to_vec();
    units.sort_by(|(left, l), (right, r)| packing_order((left.as_str(), *l), (right.as_str(), *r)));
    units
        .into_iter()
        .find(|(_, [w, d, h])| {
            i128::from(*w) > width - 2 * margin
                || i128::from(*d) > depth - 2 * margin
                || i128::from(*h) > height
        })
        .map(|(token, _)| token)
}

/// Node's `compareUnits`: longest side first, then larger footprint, then token.
fn packing_order(left: (&str, [u64; 3]), right: (&str, [u64; 3])) -> std::cmp::Ordering {
    let (left_token, [left_width, left_depth, _]) = left;
    let (right_token, [right_width, right_depth, _]) = right;
    right_width
        .max(right_depth)
        .cmp(&left_width.max(left_depth))
        .then_with(|| {
            let left_area = u128::from(left_width) * u128::from(left_depth);
            let right_area = u128::from(right_width) * u128::from(right_depth);
            right_area.cmp(&left_area)
        })
        .then_with(|| left_token.cmp(right_token))
}

fn sort_units(units: &mut [PackingUnit]) {
    let key = |unit: &PackingUnit| {
        [unit.width_um, unit.depth_um, unit.height_um].map(|value| u64::from(value.get()))
    };
    units.sort_by(|left, right| {
        packing_order(
            (left.token.as_str(), key(left)),
            (right.token.as_str(), key(right)),
        )
    });
}

pub fn pack_units(printer: &PrinterGeometry, units: &[PackingUnit]) -> PackResult {
    let mut units = units.to_vec();
    sort_units(&mut units);
    if let Some(unit) = units.iter().find(|unit| !fits(printer, unit)) {
        return PackResult::UnitTooLarge(unit.token.clone());
    }
    let mut result = Vec::new();
    let mut current = Vec::new();
    let margin = printer.margin_um.get();
    let mut x = margin;
    let mut y = margin;
    let mut row_depth = 0;
    for unit in units {
        if x > margin
            && u64::from(x) + u64::from(unit.width_um.get())
                > u64::from(printer.bed_width_um.get() - margin)
        {
            x = margin;
            y += row_depth + margin;
            row_depth = 0;
        }
        if u64::from(y) + u64::from(unit.depth_um.get())
            > u64::from(printer.bed_depth_um.get() - margin)
        {
            result.push(current);
            current = Vec::new();
            x = margin;
            y = margin;
            row_depth = 0;
        }
        let (Ok(x_um), Ok(y_um)) = (OffsetUm::parse(i64::from(x)), OffsetUm::parse(i64::from(y)))
        else {
            return PackResult::UnitTooLarge(unit.token);
        };
        current.push(PackedUnit {
            unit: unit.clone(),
            x_um,
            y_um,
        });
        x += unit.width_um.get() + margin;
        row_depth = row_depth.max(unit.depth_um.get());
    }
    if !current.is_empty() {
        result.push(current);
    }
    PackResult::Packed(result)
}

pub fn pack_units_around(
    printer: &PrinterGeometry,
    occupied: &[PackedUnit],
    units: &[PackingUnit],
) -> PackResult {
    let occupied_tokens: HashSet<_> = occupied
        .iter()
        .map(|unit| unit.unit.token.clone())
        .collect();
    let mut moving = units
        .iter()
        .filter(|unit| !occupied_tokens.contains(&unit.token))
        .cloned()
        .collect::<Vec<_>>();
    sort_units(&mut moving);
    if let Some(unit) = occupied
        .iter()
        .map(|unit| &unit.unit)
        .chain(moving.iter())
        .find(|unit| !fits(printer, unit))
    {
        return PackResult::UnitTooLarge(unit.token.clone());
    }
    if occupied.is_empty() {
        return pack_units(printer, &moving);
    }
    let mut first = occupied.to_vec();
    let mut leftover = Vec::new();
    let margin = printer.margin_um.get();
    let max_x = printer.bed_width_um.get() - margin;
    let max_y = printer.bed_depth_um.get() - margin;
    for unit in moving {
        let mut xs = vec![u64::from(margin)];
        let mut ys = vec![u64::from(margin)];
        for item in &first {
            xs.extend([
                u64::from(item.x_um.get()),
                u64::from(item.x_um.get())
                    + u64::from(item.unit.width_um.get())
                    + u64::from(margin),
            ]);
            ys.extend([
                u64::from(item.y_um.get()),
                u64::from(item.y_um.get())
                    + u64::from(item.unit.depth_um.get())
                    + u64::from(margin),
            ]);
        }
        xs.sort_unstable();
        xs.dedup();
        ys.sort_unstable();
        ys.dedup();
        let mut found = None;
        'slots: for y in ys
            .into_iter()
            .filter(|value| *value + u64::from(unit.depth_um.get()) <= u64::from(max_y))
        {
            for x in xs
                .iter()
                .copied()
                .filter(|value| *value + u64::from(unit.width_um.get()) <= u64::from(max_x))
            {
                let (Ok(x_um), Ok(y_um)) = (OffsetUm::parse(x as i64), OffsetUm::parse(y as i64))
                else {
                    return PackResult::UnitTooLarge(unit.token);
                };
                let candidate = PackedUnit {
                    unit: unit.clone(),
                    x_um,
                    y_um,
                };
                let test = UnitInput {
                    token: candidate.unit.token.clone(),
                    x_um: candidate.x_um,
                    y_um: candidate.y_um,
                    width_um: candidate.unit.width_um,
                    depth_um: candidate.unit.depth_um,
                    height_um: candidate.unit.height_um,
                    placement: Placement::Auto,
                    pinned: false,
                };
                if first.iter().all(|item| {
                    let other = UnitInput {
                        token: item.unit.token.clone(),
                        x_um: item.x_um,
                        y_um: item.y_um,
                        width_um: item.unit.width_um,
                        depth_um: item.unit.depth_um,
                        height_um: item.unit.height_um,
                        placement: Placement::Auto,
                        pinned: false,
                    };
                    !violates_clearance(&other, &test, margin)
                }) {
                    found = Some(candidate);
                    break 'slots;
                }
            }
        }
        if let Some(found) = found {
            first.push(found);
        } else {
            leftover.push(unit);
        }
    }
    match pack_units(printer, &leftover) {
        PackResult::Packed(mut extra) => {
            let mut all = vec![first];
            all.append(&mut extra);
            PackResult::Packed(all)
        }
        failure => failure,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GroupField {
    Material,
    Color,
    SourceDirectory,
    SourceLayer,
    Role,
    ObjectName,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GroupKind {
    SeparateBy,
    KeepTogether,
    SetMaterial,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GroupRule {
    pub id: String,
    pub enabled: bool,
    pub kind: GroupKind,
    pub field: GroupField,
    pub value: Option<String>,
    pub material_type: Option<String>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GroupUnit<T> {
    pub value: T,
    pub token: RequiredUnitToken,
    pub object_name: String,
    pub filename: String,
    pub source_directory: String,
    pub source_layer: String,
    pub role: String,
    pub filament_color_id: Option<String>,
    pub filament_custom_hex: Option<String>,
    pub material_type: Option<String>,
}
fn color(value: Option<&str>) -> String {
    let value = trim_ecmascript(value.unwrap_or(""));
    if value.is_empty() {
        return "unassigned".into();
    }
    let hex = value
        .strip_prefix('#')
        .unwrap_or(value)
        .to_ascii_lowercase();
    if (hex.len() == 6 || hex.len() == 8) && hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        format!("hex:{hex}")
    } else {
        format!("id:{value}")
    }
}
fn group_value<T>(unit: &GroupUnit<T>, field: &GroupField, material: Option<&str>) -> String {
    match field {
        GroupField::Material => material.unwrap_or("unassigned").to_owned(),
        GroupField::Color => color(
            unit.filament_color_id
                .as_deref()
                .filter(|value| !trim_ecmascript(value).is_empty())
                .or(unit.filament_custom_hex.as_deref()),
        ),
        GroupField::SourceDirectory => {
            if unit.source_directory.is_empty() {
                "unassigned".into()
            } else {
                unit.source_directory.clone()
            }
        }
        GroupField::SourceLayer => {
            if unit.source_layer.is_empty() {
                "unassigned".into()
            } else {
                unit.source_layer.clone()
            }
        }
        GroupField::Role => {
            if unit.role.is_empty() {
                "unassigned".into()
            } else {
                unit.role.clone()
            }
        }
        GroupField::ObjectName => {
            if unit.object_name.is_empty() {
                unit.filename.clone()
            } else {
                unit.object_name.clone()
            }
        }
    }
}
pub fn grouping_buckets<T: Clone>(units: &[GroupUnit<T>], rules: &[GroupRule]) -> Vec<Vec<T>> {
    let grouping = rules
        .iter()
        .filter(|rule| {
            rule.enabled && matches!(rule.kind, GroupKind::SeparateBy | GroupKind::KeepTogether)
        })
        .collect::<Vec<_>>();
    if grouping.is_empty() {
        return (!units.is_empty())
            .then(|| units.iter().map(|unit| unit.value.clone()).collect())
            .into_iter()
            .collect();
    }
    let mut indices = HashMap::new();
    let mut buckets: Vec<Vec<T>> = Vec::new();
    for unit in units {
        let material = rules
            .iter()
            .find(|rule| {
                rule.enabled
                    && rule.kind == GroupKind::SetMaterial
                    && group_value(unit, &rule.field, unit.material_type.as_deref())
                        == rule
                            .value
                            .as_ref()
                            .map(|value| {
                                if rule.field == GroupField::Color {
                                    color(Some(value))
                                } else {
                                    value.clone()
                                }
                            })
                            .unwrap_or_default()
            })
            .and_then(|rule| rule.material_type.as_deref())
            .or(unit.material_type.as_deref());
        let key = grouping
            .iter()
            .map(|rule| {
                let value = group_value(unit, &rule.field, material);
                match rule.kind {
                    GroupKind::SeparateBy => format!("{}:value:{value}", rule.id),
                    GroupKind::KeepTogether => {
                        if value
                            == rule
                                .value
                                .as_ref()
                                .map(|value| {
                                    if rule.field == GroupField::Color {
                                        color(Some(value))
                                    } else {
                                        value.clone()
                                    }
                                })
                                .unwrap_or_default()
                        {
                            format!("{}:match", rule.id)
                        } else {
                            format!("{}:other", rule.id)
                        }
                    }
                    GroupKind::SetMaterial => unreachable!(),
                }
            })
            .collect::<Vec<_>>()
            .join("\0");
        let index = *indices.entry(key).or_insert_with(|| {
            buckets.push(Vec::new());
            buckets.len() - 1
        });
        buckets[index].push(unit.value.clone());
    }
    buckets
}
