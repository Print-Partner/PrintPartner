use std::cell::Cell;

use serde_json::json;

use super::super::{Limits, product_structure};
use super::*;

static IDLE: AtomicBool = AtomicBool::new(false);

const NESTED: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../fixtures/step/nested-assembly.step"
));

const HEADER: &str = "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((''),'2;1');\nFILE_NAME('t','',(''),(''),'','','');\nFILE_SCHEMA(('AUTOMOTIVE_DESIGN'));\nENDSEC;\n";

fn read(text: &str) -> ProductStructure {
    product_structure(text.as_bytes(), Limits::default(), &IDLE).unwrap()
}

fn tree(structure: &ProductStructure) -> Result<PartTree, StepError> {
    part_tree(structure, TreeLimits::default(), &IDLE)
}

/// The fixture with one more occurrence before its `ENDSEC;`.
fn nested_with(record: &str) -> String {
    let end = "ENDSEC;\nEND-ISO-10303-21;";
    assert_eq!(NESTED.matches(end).count(), 1);
    NESTED.replacen(end, &format!("{record}\n{end}"), 1)
}

/// A structure built directly: definition `d` has formation `d` and product
/// `d`, whose id is `P<d>` and whose name is given. Each occurrence is
/// `(entity, parent, child)` and has no name.
fn structure(parts: &[(u64, &str)], usages: &[(u64, u64, u64)]) -> ProductStructure {
    let mut structure = ProductStructure::default();
    for &(definition, name) in parts {
        let product = (format!("P{definition}"), name.to_owned());
        structure.products.insert(definition, product);
        structure.formations.insert(definition, definition);
        structure.definitions.insert(definition, definition);
    }
    for &(entity, parent, child) in usages {
        let usage = Usage {
            instance: None,
            parent,
            child,
        };
        structure.usages.insert(entity, usage);
    }
    structure
}

/// Definitions 1..=levels, each placing the next once.
fn chain(levels: u64) -> ProductStructure {
    let parts: Vec<_> = (1..=levels).map(|d| (d, "")).collect();
    let usages: Vec<_> = (1..levels).map(|d| (1000 + d, d, d + 1)).collect();
    structure(&parts, &usages)
}

fn levels(node: &PartNode) -> usize {
    1 + node.children.iter().map(levels).max().unwrap_or(0)
}

fn count(node: &PartNode) -> usize {
    1 + node.children.iter().map(count).sum::<usize>()
}

#[test]
fn the_six_level_fixture_serializes_as_its_full_nested_tree() {
    let tree = tree(&read(NESTED)).unwrap();
    assert_eq!(tree.roots.len(), 1);
    assert_eq!(levels(&tree.roots[0]), 6);
    assert_eq!(count(&tree.roots[0]), 10);
    let leaf = |product_id: &str, name: &str, instance: &str| json!({"product_id": product_id, "name": name, "instance": instance, "children": []});
    let screw = |instance| leaf("SCR-M3", "", instance);
    let expected = json!({"roots": [{
        "product_id": "PRN-1", "name": "Printer", "instance": null, "children": [
            {"product_id": "FRM-1", "name": "Frame", "instance": "Frame:1", "children": [
                {"product_id": "GNT-1", "name": "Gantry", "instance": "Gantry:1", "children": [
                    {"product_id": "CAR-1", "name": "Carriage", "instance": "Carriage:1", "children": [
                        {"product_id": "HOT-1", "name": "Hotend", "instance": "Hotend:1", "children": [
                            leaf("NOZ-1", "D\u{fc}se 0.4", "Nozzle:1"),
                        ]},
                        screw("Screw:1"),
                        screw("Screw:2"),
                    ]},
                ]},
                // An occurrence with no name is labelled by its id.
                screw("NAUO8"),
            ]},
            leaf("BED-1", "Bed; 'heated'", "Bed:1"),
        ],
    }]});
    let text = serde_json::to_string(&tree).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&text).unwrap(),
        expected
    );
    assert_eq!(serde_json::from_str::<PartTree>(&text).unwrap(), tree);
}

/// Roots follow entity numbers, not the order the records are written in;
/// so do children, and a sub-assembly placed twice appears twice, whole.
#[test]
fn roots_and_children_follow_entity_order() {
    let part = |n: u64, name: &str| {
        let (formation, definition) = (n + 1, n + 2);
        format!(
            "#{n}=PRODUCT('{name}','{name}','',(#2));\n#{formation}=PRODUCT_DEFINITION_FORMATION('','',#{n});\n#{definition}=PRODUCT_DEFINITION('design','',#{formation},#3);\n"
        )
    };
    let usage = |n: u64, name: &str, parent: u64, child: u64| {
        format!("#{n}=NEXT_ASSEMBLY_USAGE_OCCURRENCE('U{n}','{name}','',#{parent},#{child},$);\n")
    };
    // Roots R (#32) and Q (#12); R holds sub-assembly S (#22) twice and
    // leaf L (#42) once, and S holds L.
    let body = [
        part(30, "R"),
        usage(102, "S:2", 32, 22),
        usage(101, "S:1", 32, 22),
        part(20, "S"),
        usage(103, "L:1", 22, 42),
        part(40, "L"),
        usage(100, "L:0", 32, 42),
        part(10, "Q"),
    ]
    .concat();
    let text = format!("{HEADER}DATA;\n{body}ENDSEC;\nEND-ISO-10303-21;\n");
    let tree = tree(&read(&text)).unwrap();
    let node = |name: &str, instance: Option<&str>, children: Vec<serde_json::Value>| json!({"product_id": name, "name": name, "instance": instance, "children": children});
    let sub = |instance| node("S", Some(instance), vec![node("L", Some("L:1"), vec![])]);
    let expected = json!({"roots": [
        node("Q", None, vec![]),
        node("R", None, vec![node("L", Some("L:0"), vec![]), sub("S:1"), sub("S:2")]),
    ]});
    assert_eq!(serde_json::to_value(&tree).unwrap(), expected);
}

#[test]
fn an_assembly_that_contains_itself_is_refused() {
    // The nozzle holds the frame: a cycle under the root.
    let under_root = nested_with("#109=NEXT_ASSEMBLY_USAGE_OCCURRENCE('L','','',#62,#22,$);");
    // The bed holds the printer: every definition is placed, so no root.
    let no_root = nested_with("#109=NEXT_ASSEMBLY_USAGE_OCCURRENCE('L','','',#82,#12,$);");
    // The nozzle holds itself.
    let itself = nested_with("#109=NEXT_ASSEMBLY_USAGE_OCCURRENCE('L','','',#62,#62,$);");
    for (name, text, entity) in [
        ("under the root", under_root, 22),
        ("with no root", no_root, 12),
        ("itself", itself, 62),
    ] {
        let result = tree(&read(&text));
        assert!(
            matches!(result, Err(StepError::Cycle { entity: e }) if e == entity),
            "{name}: {result:?}"
        );
    }
    // A cycle no root reaches, beside a root that is fine.
    let unreached = structure(&[(1, "R"), (2, "A"), (3, "B")], &[(10, 2, 3), (11, 3, 2)]);
    assert!(matches!(
        tree(&unreached),
        Err(StepError::Cycle { entity: 2 })
    ));
}

#[test]
fn a_file_with_no_product_definitions_is_refused() {
    let geometry =
        format!("{HEADER}DATA;\n#1=CARTESIAN_POINT('',(0.,0.,0.));\nENDSEC;\nEND-ISO-10303-21;\n");
    assert!(matches!(tree(&read(&geometry)), Err(StepError::NoProducts)));
    let unused =
        format!("{HEADER}DATA;\n#10=PRODUCT('P','P','',(#2));\nENDSEC;\nEND-ISO-10303-21;\n");
    assert!(matches!(tree(&read(&unused)), Err(StepError::NoProducts)));
}

/// Each default cap accepts a tree at the cap and refuses one unit more.
#[test]
fn each_tree_limit_accepts_its_cap_and_refuses_one_more() {
    let refused = |structure: &ProductStructure| match tree(structure) {
        Err(StepError::TooLarge(limit)) => limit,
        other => panic!("{other:?}"),
    };

    // 60 levels, whose JSON serde_json reads back within its nesting limit.
    let deepest = tree(&chain(60)).unwrap();
    assert_eq!(levels(&deepest.roots[0]), 60);
    let text = serde_json::to_string(&deepest).unwrap();
    assert_eq!(serde_json::from_str::<PartTree>(&text).unwrap(), deepest);
    assert_eq!(refused(&chain(61)), Limit::Levels);

    // A root and one leaf placed 99,999 or 100,000 times.
    let fan = |placements: u64| {
        let usages: Vec<_> = (0..placements).map(|n| (100 + n, 1, 2)).collect();
        structure(&[(1, ""), (2, "")], &usages)
    };
    assert_eq!(count(&tree(&fan(99_999)).unwrap().roots[0]), 100_000);
    assert_eq!(refused(&fan(100_000)), Limit::Nodes);

    // A leaf with a 1 MiB name and an empty id placed 32 times: 32 MiB. A
    // one-byte root name is one byte more.
    let names = |root: &str| {
        let mut structure = structure(&[(1, root), (2, &"x".repeat(1 << 20))], &[]);
        for n in 0..32 {
            structure.usages.insert(
                100 + n,
                Usage {
                    instance: None,
                    parent: 1,
                    child: 2,
                },
            );
        }
        for product in structure.products.values_mut() {
            product.0.clear();
        }
        structure
    };
    assert_eq!(tree(&names("")).unwrap().roots[0].children.len(), 32);
    assert_eq!(refused(&names("x")), Limit::NameBytes);
}

/// Hostile shapes stop at a cap without deep recursion or unbounded work.
#[test]
fn hostile_shapes_stop_at_a_cap() {
    // 200,000 levels: the cycle search is iterative and the build stops at
    // level 61.
    assert!(matches!(
        tree(&chain(200_000)),
        Err(StepError::TooLarge(Limit::Levels))
    ));
    // 40 definitions each placing the next twice would be 2^40 - 1 nodes.
    let parts: Vec<_> = (1..=40).map(|d| (d, "")).collect();
    let usages: Vec<_> = (1..40)
        .flat_map(|d| [(1000 + 2 * d, d, d + 1), (1001 + 2 * d, d, d + 1)])
        .collect();
    let doubling = structure(&parts, &usages);
    assert!(matches!(
        tree(&doubling),
        Err(StepError::TooLarge(Limit::Nodes))
    ));
}

/// The flag is checked once per definition in the cycle search (8) and once
/// per node (10); setting it at any check ends the build at that check.
#[test]
fn cancellation_is_checked_per_node() {
    let structure = read(NESTED);
    let checks = Cell::new(0);
    build(&structure, TreeLimits::default(), || {
        checks.set(checks.get() + 1);
        false
    })
    .unwrap();
    assert_eq!(checks.get(), 18);
    for at in 1..=18 {
        let checks = Cell::new(0);
        let result = build(&structure, TreeLimits::default(), || {
            checks.set(checks.get() + 1);
            checks.get() == at
        });
        assert!(matches!(result, Err(StepError::Cancelled)), "at {at}");
        assert_eq!(checks.get(), at);
    }
    let cancelled = AtomicBool::new(true);
    let result = part_tree(&structure, TreeLimits::default(), &cancelled);
    assert!(matches!(result, Err(StepError::Cancelled)));
}
