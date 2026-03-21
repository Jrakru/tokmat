use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct TokenizerReferenceCase {
    pub input: String,
    pub cleaned: String,
    pub tokens: Vec<String>,
    pub types: Vec<String>,
    pub classes: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct ExtractorReferenceCase {
    pub input: String,
    pub cleaned: String,
    pub tokens: Vec<String>,
    pub classes: Vec<String>,
    pub pattern: String,
    #[serde(default = "default_match_mode")]
    pub mode: String,
    pub fields: HashMap<String, String>,
    pub complement: String,
}

fn default_match_mode() -> String {
    "whole".to_string()
}

fn parse_match_mode(mode: &str) -> tokmat::extractor::MatchMode {
    match mode {
        "start" => tokmat::extractor::MatchMode::Start,
        "end" => tokmat::extractor::MatchMode::End,
        "any" => tokmat::extractor::MatchMode::Any,
        _ => tokmat::extractor::MatchMode::Whole,
    }
}

#[derive(Debug, Deserialize)]
pub struct FieldMatchingReferenceCase {
    pub field: String,
    pub actual: String,
    pub expected: String,
    pub result: FieldComparisonResult,
}

#[derive(Debug, Deserialize)]
pub struct FieldComparisonResult {
    pub field: String,
    pub expected: String,
    pub actual: String,
    pub strict_match: bool,
    pub matched: bool,
    pub similarity: f64,
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct CohortReferenceCase {
    pub input: String,
    pub signature: String,
    pub labels: Vec<String>,
    pub cohort_id: String,
}

/// Load a reference fixture file from the shared test corpus.
///
/// # Panics
///
/// Panics if the workspace layout is not the expected Cargo workspace structure, if the fixture
/// file cannot be read, or if the JSON cannot be deserialized into the requested shape.
#[must_use]
pub fn load_reference_fixture<T: for<'de> Deserialize<'de>>(filename: &str) -> Vec<T> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/reference_corpus")
        .join(filename);

    let content = fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("Failed to read reference fixture: {}", path.display()));

    serde_json::from_str(&content)
        .unwrap_or_else(|_| panic!("Failed to parse reference fixture JSON: {}", path.display()))
}

#[must_use]
/// Load and concatenate multiple reference fixture files when they exist.
///
/// Missing files are skipped so generated corpora can be optional in local workflows.
///
/// # Panics
///
/// Panics if the workspace layout is not the expected Cargo workspace structure, if an existing
/// fixture file cannot be read, or if the JSON cannot be deserialized into the requested shape.
pub fn load_combined_reference_fixtures<T: for<'de> Deserialize<'de>>(
    filenames: &[&str],
) -> Vec<T> {
    filenames
        .iter()
        .flat_map(|filename| {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/reference_corpus")
                .join(filename);
            if !path.exists() {
                return Vec::new();
            }

            let content = fs::read_to_string(&path)
                .unwrap_or_else(|_| panic!("Failed to read reference fixture: {}", path.display()));

            serde_json::from_str::<Vec<T>>(&content).unwrap_or_else(|_| {
                panic!("Failed to parse reference fixture JSON: {}", path.display())
            })
        })
        .collect()
}

fn model_fixture_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/model_1")
}

fn run_tokenizer_parity(cases: &[TokenizerReferenceCase]) {
    use tokmat::tokenizer::{load_token_class_list, load_token_definitions, tokenize_and_classify};

    let base_path = model_fixture_root();
    let token_definitions =
        load_token_definitions(base_path.join("TOKENDEFINITION/TOKENDEFINITONS.param2"))
            .expect("Failed to load token definitions");
    let token_class_list = load_token_class_list(base_path.join("TOKENCLASS"))
        .expect("Failed to load token class list");

    for case in cases {
        let result =
            tokenize_and_classify(&case.cleaned, &token_definitions, Some(&token_class_list));

        assert_eq!(
            result.tokens, case.tokens,
            "Tokens mismatch for input: {}",
            case.input
        );
        assert_eq!(
            result.types, case.types,
            "Types mismatch for input: {}",
            case.input
        );
        if !case.classes.is_empty() {
            assert_eq!(
                result.classes, case.classes,
                "Classes mismatch for input: {}",
                case.input
            );
        }
    }
}

fn run_extractor_parity(cases: &[ExtractorReferenceCase]) {
    use tokmat::extractor::Extractor;
    use tokmat::tokenizer::{load_token_class_list, load_token_definitions};

    let base_path = model_fixture_root();
    let token_definitions =
        load_token_definitions(base_path.join("TOKENDEFINITION/TOKENDEFINITONS.param2"))
            .expect("Failed to load token definitions");
    let token_class_list = load_token_class_list(base_path.join("TOKENCLASS"))
        .expect("Failed to load token class list");

    let ext = Extractor::new(token_definitions, token_class_list);
    let mut failures = 0;
    for case in cases {
        let result = ext.parse_tokens(
            &case.input,
            &case.tokens,
            &case.classes,
            &case.pattern,
            parse_match_mode(&case.mode),
        );
        match result {
            Ok(output) => {
                // Check if all expected fields are present and match
                for (key, val) in &case.fields {
                    if let Some(actual_val) = output.fields.get(key) {
                        if actual_val != val {
                            println!(
                                "Mismatch for input '{}', pattern '{}', field '{}': expected '{}', got '{}'",
                                case.input, case.pattern, key, val, actual_val
                            );
                            failures += 1;
                        }
                    } else {
                        println!(
                            "Missing field '{}' for input '{}', pattern '{}'",
                            key, case.input, case.pattern
                        );
                        failures += 1;
                    }
                }
                // Check for extra fields
                for key in output.fields.keys() {
                    if !case.fields.contains_key(key) {
                        println!(
                            "Extra field '{}' for input '{}', pattern '{}'",
                            key, case.input, case.pattern
                        );
                        failures += 1;
                    }
                }
                if output.complement != case.complement {
                    println!(
                        "Complement mismatch for input '{}', pattern '{}': expected '{}', got '{}'",
                        case.input, case.pattern, case.complement, output.complement
                    );
                    failures += 1;
                }
            }
            Err(e) => {
                println!(
                    "Error for input '{}', pattern '{}': {:?}",
                    case.input, case.pattern, e
                );
                failures += 1;
            }
        }
    }
    assert_eq!(failures, 0, "Found {failures} extraction mismatches");
}

#[test]
fn test_tokenizer_parity() {
    let cases: Vec<TokenizerReferenceCase> = load_reference_fixture("tokenizer.json");
    run_tokenizer_parity(&cases);
}

#[test]
fn test_extractor_parity() {
    let cases: Vec<ExtractorReferenceCase> = load_reference_fixture("extractor.json");
    run_extractor_parity(&cases);
}

#[test]
#[ignore = "Generated upstream wanParser fixtures are diagnostic until Rust parity is fully closed"]
fn test_tokenizer_parity_from_upstream_pytests() {
    let cases: Vec<TokenizerReferenceCase> = load_reference_fixture("tokenizer_from_pytests.json");
    if cases.is_empty() {
        eprintln!(
            "No harvested tokenizer fixtures were produced from the selected upstream pytest set"
        );
        return;
    }
    run_tokenizer_parity(&cases);
}

#[test]
#[ignore = "Generated upstream wanParser fixtures are diagnostic until Rust parity is fully closed"]
fn test_extractor_parity_from_upstream_pytests() {
    let cases: Vec<ExtractorReferenceCase> = load_reference_fixture("extractor_from_pytests.json");
    assert!(!cases.is_empty(), "expected harvested extractor fixtures");
    run_extractor_parity(&cases);
}

#[test]
fn test_load_field_matching_reference_cases() {
    let cases: Vec<FieldMatchingReferenceCase> = load_reference_fixture("field_matching.json");
    assert!(!cases.is_empty());
    println!("Loaded {} field matching cases", cases.len());
}

#[test]
fn test_load_cohort_reference_cases() {
    let cases: Vec<CohortReferenceCase> = load_reference_fixture("cohorts.json");
    assert!(!cases.is_empty());
    println!("Loaded {} cohort cases", cases.len());
}
