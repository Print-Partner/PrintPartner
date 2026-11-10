#![cfg(feature = "desktop_rebuild")]
//! GRE-425 (T-58-kit): replays the Node kit bundle golden in
//! `fixtures/parity/kit-bundle`. Regenerate it with
//! `node fixtures/parity/kit-bundle/generate.mjs`.

mod ticket_t_58_kit {
    use std::io::{Cursor, Write};

    use pp_services::kit::source_refs::{
        KitSourceProject, KitSourceRef, collect_kit_bundle_source_refs,
    };
    use pp_services::kit::{
        EditableKitRecipe, KitBundleError, KitBundlePart, MAX_KIT_JSON_BYTES,
        build_editable_kit_bundle, kit_bundle_zip, kit_json_text, parse_kit_bundle, stringify,
    };
    use serde_json::{Map, Value, json};
    use sha2::{Digest, Sha256};

    fn golden() -> Value {
        serde_json::from_str(include_str!(
            "../../../../fixtures/parity/kit-bundle/kit-bundle.json"
        ))
        .unwrap()
    }

    /// The padded `kit.json` from `export-kit.test.ts`.
    fn padded(total: usize) -> Vec<u8> {
        let prefix = br#"{"format":"print-partner-kit","version":3,"padding":""#;
        let mut out = prefix.to_vec();
        out.resize(total - 2, b'a');
        out.extend_from_slice(br#""}"#);
        out
    }

    /// A stored kit zip whose headers claim `declared` uncompressed bytes.
    fn stored_understated(payload: &[u8], declared: u32) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .large_file(false);
        writer.start_file("kit.json", options).unwrap();
        writer.write_all(payload).unwrap();
        let mut bytes = writer.finish().unwrap().into_inner();
        let local = bytes.windows(4).position(|w| w == b"PK\x03\x04").unwrap();
        let central = bytes.windows(4).rposition(|w| w == b"PK\x01\x02").unwrap();
        bytes[local + 22..local + 26].copy_from_slice(&declared.to_le_bytes());
        bytes[central + 24..central + 28].copy_from_slice(&declared.to_le_bytes());
        bytes
    }

    fn input_bytes(case: &Value) -> Vec<u8> {
        if let Some(hex) = case["hex"].as_str() {
            return hex::decode(hex).unwrap();
        }
        let recipe = &case["recipe"];
        let payload = padded(recipe["padded"].as_u64().unwrap() as usize);
        let Some(declared) = recipe["stored_declared"].as_u64() else {
            return payload;
        };
        let mut bytes = stored_understated(&payload, declared as u32);
        if recipe["flip_version"] == json!(true) {
            // Flipping a stored byte breaks the entry's CRC.
            let at = bytes.windows(9).position(|w| w == b"\"version\"").unwrap();
            bytes[at] ^= 0x20;
        }
        bytes
    }

    /// Node's result as JSON text, with large documents reduced to the
    /// SHA-256 of their `JSON.stringify` text.
    fn node_ok_text(ok: &Value) -> String {
        stringify(ok, false)
    }

    fn rust_ok_text(data: &Map<String, Value>) -> String {
        let text = stringify(&Value::Object(data.clone()), false);
        if text.len() > 1024 * 1024 {
            let digest = hex::encode(Sha256::digest(text.as_bytes()));
            return stringify(&json!({ "sha256": digest }), false);
        }
        text
    }

    #[test]
    fn ticket_t_58_kit_upload_parse_matches_node() {
        let golden = golden();
        assert_eq!(
            golden["max_kit_json_bytes"].as_u64(),
            Some(MAX_KIT_JSON_BYTES)
        );
        let cases = golden["parse"].as_array().unwrap();
        assert_eq!(cases.len(), 57);
        for case in cases {
            let id = case["id"].as_str().unwrap();
            let bytes = input_bytes(case);
            let rust = parse_kit_bundle(&bytes, case["filename"].as_str());
            let node = &case["result"];
            match (&rust, node.get("ok")) {
                (Ok(data), Some(ok)) => assert_eq!(rust_ok_text(data), node_ok_text(ok), "{id}"),
                (Err(error), None) => {
                    let expected = &node["error"];
                    match expected["kind"].as_str().unwrap() {
                        "too_large" => assert_eq!(error, &KitBundleError::TooLarge, "{id}"),
                        "message" => assert_eq!(
                            error.to_string(),
                            expected["message"].as_str().unwrap(),
                            "{id}"
                        ),
                        // V8 and adm-zip word these; Rust reports serde_json's and zip's text.
                        "syntax" => {
                            assert!(matches!(error, KitBundleError::Json(_)), "{id}: {error:?}")
                        }
                        "archive" => {
                            assert!(matches!(error, KitBundleError::Zip(_)), "{id}: {error:?}")
                        }
                        kind => panic!("{id}: unexpected kind {kind}"),
                    }
                }
                _ => panic!("{id}: Rust {rust:?}, Node {node}"),
            }
        }
    }

    /// The fields `KitBundleSourceRef` serializes to in Node (`layer_type`
    /// only when set).
    fn ref_json(source: &KitSourceRef) -> Value {
        let mut out = json!({
            "name": source.name,
            "url": source.url,
            "branch": source.branch,
            "tag": source.tag,
            "source_kind": source.source_kind,
            "source_type": source.source_type,
            "role": source.role,
            "category": source.category,
            "import_rules": source.import_rules,
            "manifest_community_slug": source.manifest_community_slug,
        });
        if let Some(layer) = &source.layer_type {
            out["layer_type"] = json!(layer);
        }
        out
    }

    #[test]
    fn ticket_t_58_kit_source_refs_match_node() {
        let golden = golden();
        let cases = golden["source_refs"].as_array().unwrap();
        assert_eq!(cases.len(), 13);
        for case in cases {
            let id = case["id"].as_str().unwrap();
            let rust = collect_kit_bundle_source_refs(case["data"].as_object().unwrap());
            let node = &case["result"];
            match (rust, node.get("ok")) {
                (Ok(refs), Some(ok)) => {
                    let rust = json!({
                        "refs": refs.iter().map(ref_json).collect::<Vec<_>>(),
                        "unmatched": refs.iter().map(KitSourceRef::to_unmatched).collect::<Vec<_>>(),
                        "patches": refs.iter().map(KitSourceRef::matched_patch).collect::<Vec<_>>(),
                        "exports": refs.iter().map(KitSourceRef::to_export_record).collect::<Vec<_>>(),
                    });
                    assert_eq!(stringify(&rust, false), stringify(ok, false), "{id}");
                }
                (Err(error), None) => match node["error"]["kind"].as_str().unwrap() {
                    "invalid_slug" => assert_eq!(
                        error.to_string(),
                        node["error"]["message"].as_str().unwrap(),
                        "{id}"
                    ),
                    // V8 names its own source expression; Rust names the field.
                    "type" => assert!(
                        error.to_string().starts_with("kit bundle "),
                        "{id}: {error}"
                    ),
                    kind => panic!("{id}: unexpected kind {kind}"),
                },
                (rust, _) => panic!("{id}: Rust {rust:?}, Node {node}"),
            }
        }
    }

    fn project(row: &Value) -> KitSourceProject {
        let text = |key: &str| row[key].as_str().map(str::to_owned);
        KitSourceProject {
            name: text("name").unwrap(),
            url: text("url").unwrap(),
            branch: text("branch"),
            tag: text("tag"),
            source_kind: text("sourceKind"),
            source_type: text("sourceType"),
            role: text("role"),
            imported_paths: text("importedPaths"),
            metadata_json: text("metadataJson"),
            manifest_community_slug: text("manifestCommunitySlug"),
        }
    }

    #[test]
    fn ticket_t_58_kit_project_export_records_match_node() {
        let golden = golden();
        let cases = golden["projects"].as_array().unwrap();
        assert_eq!(cases.len(), 13);
        for case in cases {
            let id = case["id"].as_str().unwrap();
            let source = KitSourceRef::from_project(&project(&case["row"]));
            let (export, patch) = (source.to_export_record(), source.matched_patch());
            // Declared difference: Node looks the lowercased role up on a plain
            // object, so "constructor" and "__proto__" resolve to members of
            // Object.prototype. JSON.stringify drops the function category (or
            // writes the prototype as {}), and the patch gets that metadata.
            // Rust treats both as unknown roles.
            let prototype_category = match id {
                "role_constructor" => None,
                "role___proto__" => Some(json!({})),
                _ => {
                    assert_eq!(
                        stringify(&export, false),
                        stringify(&case["export"], false),
                        "{id}"
                    );
                    assert_eq!(
                        stringify(&patch, false),
                        stringify(&case["patch"], false),
                        "{id}"
                    );
                    continue;
                }
            };
            assert_eq!(
                case["export"].get("category"),
                prototype_category.as_ref(),
                "{id}"
            );
            let node_metadata =
                prototype_category.map_or_else(|| json!({}), |c| json!({ "category": c }));
            assert_eq!(case["patch"]["metadata"], node_metadata, "{id}");
            assert_eq!(export["category"], json!("unassigned"), "{id}");
            assert!(patch.get("metadata").is_none(), "{id}");
            let mut node_export = case["export"].clone();
            node_export["category"] = json!("unassigned");
            let mut node_patch = case["patch"].clone();
            node_patch.as_object_mut().unwrap().remove("metadata");
            assert_eq!(export, node_export, "{id}");
            assert_eq!(patch, node_patch, "{id}");
        }
    }

    fn recipe(raw: &Value) -> EditableKitRecipe {
        let text = |value: &Value| value.as_str().map(str::to_owned);
        let parts = raw["workingParts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|part| KitBundlePart {
                match_key: text(&part["matchKey"]).unwrap(),
                relative_path: text(&part["relativePath"]).unwrap(),
                filename: text(&part["filename"]).unwrap(),
                source_layer: text(&part["sourceLayer"]).unwrap(),
                status: text(&part["status"]).unwrap(),
                role: text(&part["role"]).unwrap(),
                filament_color_id: text(&part["filamentColorId"]),
                filament_custom_hex: text(&part["filamentCustomHex"]),
                quantity_inferred: part["quantityInferred"].as_i64().unwrap(),
                quantity_override: part["quantityOverride"].as_i64(),
                quantity_effective: part["quantityEffective"].as_i64().unwrap(),
                included: part["included"].as_bool().unwrap(),
                notes: text(&part["notes"]).unwrap(),
                geometry_same: part["geometrySame"].as_bool(),
                requirement: text(&part["requirement"]),
                option_group_id: text(&part["optionGroupId"]),
                manifest_source: text(&part["manifestSource"]),
            });
        EditableKitRecipe {
            profile_name: text(&raw["profile"]["name"]).unwrap(),
            order_number: text(&raw["profile"]["orderNumber"]),
            layers: raw["layers"].as_array().unwrap().clone(),
            sources: raw["sources"].as_array().unwrap().clone(),
            kit_manifest: raw["kitManifest"].clone(),
            working_parts: parts.collect(),
        }
    }

    #[test]
    fn ticket_t_58_kit_bundle_build_and_round_trip_match_node() {
        let golden = golden();
        let build = &golden["build"];
        assert_eq!(build["round_trip_equal"], json!(true));
        let node_text = build["kit_json"].as_str().unwrap();
        let data = build_editable_kit_bundle(
            &recipe(&build["recipe"]),
            build["exported_at"].as_str().unwrap(),
        );

        // kit.json is byte-identical to Node's JSON.stringify(data, null, 2).
        assert_eq!(kit_json_text(&data), node_text);

        // Node's zip parses to the same document in Rust.
        let node_zip = hex::decode(build["zip_hex"].as_str().unwrap()).unwrap();
        let from_node = parse_kit_bundle(&node_zip, Some("x.print-partner-kit.zip")).unwrap();
        assert_eq!(kit_json_text(&Value::Object(from_node.clone())), node_text);

        // Rust's zip reads back to the same document, by name and by magic.
        let rust_zip = kit_bundle_zip(&data).unwrap();
        for filename in [
            Some("bundle.print-partner-kit.zip"),
            Some("upload.bin"),
            None,
        ] {
            let back = parse_kit_bundle(&rust_zip, filename).unwrap();
            assert_eq!(Value::Object(back), Value::Object(from_node.clone()));
        }
    }

    /// Bundles carry source references only: no model files go into the zip,
    /// and a parsed bundle exposes no file bytes.
    #[test]
    fn ticket_t_58_kit_bundle_holds_only_kit_json() {
        let golden = golden();
        let build = &golden["build"];
        let data = build_editable_kit_bundle(&recipe(&build["recipe"]), "2026-10-30T01:02:03.004Z");
        let bytes = kit_bundle_zip(&data).unwrap();
        let archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        assert_eq!(
            archive
                .file_names()
                .map(|name| name.unwrap().into_owned())
                .collect::<Vec<_>>(),
            vec!["kit.json"]
        );

        let node_zip = hex::decode(build["zip_hex"].as_str().unwrap()).unwrap();
        let archive = zip::ZipArchive::new(Cursor::new(node_zip)).unwrap();
        assert_eq!(
            archive
                .file_names()
                .map(|name| name.unwrap().into_owned())
                .collect::<Vec<_>>(),
            vec!["kit.json"]
        );
    }
}
