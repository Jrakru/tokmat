use std::path::Path;

use tokmat::extractor::{Extractor, MatchMode};
use tokmat::tokenizer::{load_token_class_list, load_token_definitions, split_input_tokens};

fn load_extractor() -> Extractor {
    let base_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/model_1");

    let token_definitions =
        load_token_definitions(base_path.join("TOKENDEFINITION/TOKENDEFINITONS.param2"))
            .expect("token definitions");
    let token_class_list =
        load_token_class_list(base_path.join("TOKENCLASS")).expect("token class list");
    Extractor::new(token_definitions, token_class_list)
}

#[test]
fn test_tokenizer_keeps_extended_word_boundaries() {
    assert_eq!(
        split_input_tokens("APT-210 O'CONNOR"),
        vec!["APT-210", " ", "O'CONNOR"]
    );
}

#[test]
fn test_extractor_requires_percent_for_extended_alpha() {
    let extractor = load_extractor();
    let tokens = vec!["Jean-Paul".to_string()];
    let classes = vec!["ALPHA_EXTENDED".to_string()];

    let without_percent = extractor
        .parse_tokens("1", &tokens, &classes, "<<NAME@>>", MatchMode::Whole)
        .expect("pattern should parse");
    assert!(without_percent.fields.is_empty());

    let with_percent = extractor
        .parse_tokens("1", &tokens, &classes, "<<NAME@%>>", MatchMode::Whole)
        .expect("pattern should parse");
    assert_eq!(
        with_percent.fields.get("NAME").map(String::as_str),
        Some("Jean-Paul")
    );
}

#[test]
fn test_extractor_preserves_leading_space_in_complement() {
    let extractor = load_extractor();
    let output = extractor
        .parse_tokens(
            "1",
            &[" ".to_string(), "NS".to_string()],
            &[" ".to_string(), "PROV".to_string()],
            "<<PROV::PROV>>",
            MatchMode::Whole,
        )
        .expect("pattern should parse");

    assert_eq!(output.fields.get("PROV").map(String::as_str), Some("NS"));
    assert_eq!(output.complement, " ");
}

#[test]
fn test_extractor_handles_literal_quotes() {
    let extractor = load_extractor();
    let output = extractor
        .parse_tokens(
            "1",
            &[
                "NOAH".to_string(),
                " ".to_string(),
                "\"".to_string(),
                "AWESOME".to_string(),
                "\"".to_string(),
                " ".to_string(),
                "STEVENS".to_string(),
            ],
            &[
                "ALPHA".to_string(),
                " ".to_string(),
                "\"".to_string(),
                "ALPHA".to_string(),
                "\"".to_string(),
                " ".to_string(),
                "ALPHA".to_string(),
            ],
            "<<FIRST>> \"<<TITLE>>\" <<LAST>>",
            MatchMode::Whole,
        )
        .expect("pattern should parse");

    assert_eq!(output.fields.get("FIRST").map(String::as_str), Some("NOAH"));
    assert_eq!(
        output.fields.get("TITLE").map(String::as_str),
        Some("AWESOME")
    );
    assert_eq!(
        output.fields.get("LAST").map(String::as_str),
        Some("STEVENS")
    );
}
