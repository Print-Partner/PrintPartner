//! T-9: shared semantic corpora for the API types, with the frozen autosave corpus.
use pp_api_types::{
    parse_identity, parse_receipt, parse_request, semantics::parse_semantic_fixture,
};
use serde_json::Value;

#[test]
fn ticket_t_9_semantics_shared_positive_negative_corpus() {
    let corpus: Value = serde_json::from_str(include_str!(
        "../../../../web/packages/contracts/test-fixtures/t9/semantics.json"
    ))
    .unwrap();
    let cases = corpus["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 34);
    for case in cases {
        let result = parse_semantic_fixture(&case["input"].to_string());
        assert_eq!(
            result.is_ok(),
            case["accepted"].as_bool().unwrap(),
            "{}: {result:?}",
            case["name"]
        );
        if let Ok(output) = result {
            assert_eq!(output, case["output"], "{}", case["name"]);
        }
    }
}

#[test]
fn ticket_t_9_semantics_frozen_autosave_corpus() {
    let corpus: Value = serde_json::from_str(include_str!(
        "../../../../web/packages/contracts/test-fixtures/desktop/autosave-v1.json"
    ))
    .unwrap();
    let cases = corpus["normalized"]["schema_cases"].as_array().unwrap();
    assert_eq!(cases.len(), 41);
    for case in cases {
        let text = case["input"].to_string();
        let result = match case["parser"].as_str().unwrap() {
            "parseSavePlanChoicesRequest" => {
                parse_request(&text).map(|value| serde_json::to_value(value).unwrap())
            }
            "parseApplyPlanDraftReceipt" => {
                parse_receipt(&text).map(|value| serde_json::to_value(value).unwrap())
            }
            "parsePlanDraftIdentity" => {
                parse_identity(&text).map(|value| serde_json::to_value(value).unwrap())
            }
            other => panic!("Unsupported corpus parser: {other}"),
        };
        assert_eq!(
            result.is_ok(),
            case["outcome"]["kind"] == "accepted",
            "{}: {result:?}",
            case["name"]
        );
        if let Ok(parsed) = result {
            assert_eq!(parsed, case["outcome"]["parsed"], "{}", case["name"]);
        }
    }
}

#[test]
fn ticket_t_9_input_output_schema_artifacts_are_distinct() {
    let input: Value = serde_json::from_str(include_str!(
        "../../../../web/scripts/desktop-contracts/schemas/semantic-fixture.input.schema.json"
    ))
    .unwrap();
    let output: Value = serde_json::from_str(include_str!(
        "../../../../web/scripts/desktop-contracts/schemas/semantic-fixture.output.schema.json"
    ))
    .unwrap();
    assert_ne!(input, output);
    assert!(
        !input["required"]
            .as_array()
            .unwrap()
            .contains(&Value::from("defaulted"))
    );
    assert!(
        output["required"]
            .as_array()
            .unwrap()
            .contains(&Value::from("defaulted"))
    );
    assert!(
        output["required"]
            .as_array()
            .unwrap()
            .contains(&Value::from("nullable"))
    );
}
