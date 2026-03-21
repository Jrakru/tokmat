//! Tokenization helpers for the wanParser Rust port.

use crate::token_model::TokenModel;
use pcre2::bytes::{Regex as Pcre2Regex, RegexBuilder as Pcre2RegexBuilder};
use std::collections::{HashMap, HashSet};
use std::hash::BuildHasher;
use std::sync::LazyLock;

pub use crate::token_model::{
    TokenClassList, TokenDefinition, load_token_class_list, load_token_definitions,
};

static WORD_BOUNDARY_RE: LazyLock<Pcre2Regex> =
    LazyLock::new(|| compile_token_regex(r"(?:(?=[\w\-'])(?<![\w\-'])|(?<=[\w\-'])(?![\w\-']))"));

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenizedResult {
    pub raw_value: String,
    pub tokens: Vec<String>,
    pub types: Vec<String>,
    pub classes: Vec<String>,
}

/// Split an input string using the wanParser word-boundary definition.
///
/// # Panics
///
/// Panics if the built-in PCRE2 word-boundary regex cannot execute, which would indicate an
/// internal invariant violation because the pattern is compiled during crate initialization.
#[must_use]
pub fn split_input_tokens(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut segment_start = 0_usize;

    for boundary in WORD_BOUNDARY_RE.find_iter(input.as_bytes()) {
        let boundary = boundary.expect("word boundary regex should execute");
        let boundary_index = boundary.start();
        if boundary_index > segment_start {
            tokens.push(input[segment_start..boundary_index].to_string());
        }
        segment_start = boundary_index;
    }

    if segment_start < input.len() {
        tokens.push(input[segment_start..].to_string());
    }

    tokens
}

/// Fast-path classifier mirroring the Python tokenizer shortcuts.
#[must_use]
pub fn get_token_fast_classifier<S: BuildHasher>(
    token: &str,
    available_names: &HashSet<String, S>,
) -> Option<String> {
    if token.is_empty() {
        return None;
    }

    if token.is_ascii() && available_names.contains("POSTALCODE") {
        let compact: String = token
            .chars()
            .filter(|character| {
                !character.is_whitespace() && *character != '-' && *character != '_'
            })
            .collect();
        let chars: Vec<char> = compact.chars().collect();
        if chars.len() == 6
            && chars[0].is_ascii_alphabetic()
            && chars[1].is_ascii_digit()
            && chars[2].is_ascii_alphabetic()
            && chars[3].is_ascii_digit()
            && chars[4].is_ascii_alphabetic()
            && chars[5].is_ascii_digit()
        {
            return Some("POSTALCODE".to_string());
        }
    }

    if token.is_ascii()
        && token.chars().all(|character| character.is_ascii_digit())
        && available_names.contains("NUM")
    {
        return Some("NUM".to_string());
    }

    if token.is_ascii()
        && token.chars().all(char::is_alphabetic)
        && available_names.contains("ALPHA")
    {
        return Some("ALPHA".to_string());
    }

    if token.is_ascii()
        && token
            .chars()
            .all(|character| character.is_ascii_digit() || character == '-')
        && token.chars().any(|character| character.is_ascii_digit())
        && available_names.contains("NUM_EXTENDED")
    {
        return Some("NUM_EXTENDED".to_string());
    }

    if token.is_ascii()
        && token
            .chars()
            .all(|character| character.is_alphabetic() || character == '-' || character == '\'')
        && token.chars().any(char::is_alphabetic)
        && available_names.contains("ALPHA_EXTENDED")
    {
        return Some("ALPHA_EXTENDED".to_string());
    }

    if token.is_ascii()
        && token
            .chars()
            .all(|character| character.is_alphanumeric() || character == '-' || character == '\'')
    {
        let has_alpha = token.chars().any(char::is_alphabetic);
        let has_digit = token.chars().any(|character| character.is_ascii_digit());
        if has_alpha && has_digit {
            if token.chars().all(char::is_alphanumeric) && available_names.contains("ALPHA_NUM") {
                return Some("ALPHA_NUM".to_string());
            }
            if available_names.contains("ALPHA_NUM_EXTENDED") {
                return Some("ALPHA_NUM_EXTENDED".to_string());
            }
        }
    }

    None
}

/// Tokenize and classify a cleaned wanParser string.
///
/// # Panics
///
/// Panics if a token definition contains a regex pattern that compiled successfully when the
/// model was loaded but cannot be recompiled with start/end anchors applied here.
#[must_use]
pub fn tokenize_and_classify(
    raw_value: &str,
    token_definitions: &TokenDefinition,
    token_class_list: Option<&TokenClassList>,
) -> TokenizedResult {
    let tokens = split_input_tokens(raw_value);
    let available_names: HashSet<String> = token_definitions
        .iter()
        .map(|(name, _)| name.clone())
        .collect();
    let compiled_patterns: Vec<(String, Pcre2Regex)> = token_definitions
        .iter()
        .map(|(name, pattern)| {
            let anchored = if pattern.starts_with('^') && pattern.ends_with('$') {
                pattern.clone()
            } else {
                format!(
                    "^{}$",
                    pattern.trim_start_matches('^').trim_end_matches('$')
                )
            };
            (name.clone(), compile_token_regex(&anchored))
        })
        .collect();

    let token_class_lookup = build_token_class_lookup(token_class_list);
    let mut types = Vec::with_capacity(tokens.len());
    let mut classes = Vec::with_capacity(tokens.len());

    for token in &tokens {
        let token_type = get_token_fast_classifier(token, &available_names).unwrap_or_else(|| {
            compiled_patterns
                .iter()
                .find_map(|(name, regex)| {
                    regex
                        .is_match(token.as_bytes())
                        .ok()
                        .and_then(|matched| matched.then(|| name.clone()))
                })
                .unwrap_or_else(|| token.clone())
        });

        types.push(token_type.clone());

        if token_class_list.is_some() {
            if token.chars().all(char::is_whitespace) {
                classes.push(token.clone());
            } else {
                classes.push(token_class_lookup.get(token).cloned().unwrap_or(token_type));
            }
        }
    }

    TokenizedResult {
        raw_value: raw_value.to_string(),
        tokens,
        types,
        classes,
    }
}

/// Tokenize using a precompiled [`TokenModel`].
#[must_use]
pub fn tokenize_with_model(raw_value: &str, model: &TokenModel) -> TokenizedResult {
    let tokens = split_input_tokens(raw_value);
    let mut types = Vec::with_capacity(tokens.len());
    let mut classes = Vec::with_capacity(tokens.len());

    for token in &tokens {
        let token_type =
            get_token_fast_classifier(token, model.available_names()).unwrap_or_else(|| {
                model
                    .compiled_patterns()
                    .iter()
                    .find_map(|(name, regex)| {
                        regex
                            .is_match(token.as_bytes())
                            .ok()
                            .and_then(|matched| matched.then(|| name.clone()))
                    })
                    .unwrap_or_else(|| token.clone())
            });

        types.push(token_type.clone());
        if token.chars().all(char::is_whitespace) {
            classes.push(token.clone());
        } else {
            classes.push(
                model
                    .token_class_lookup()
                    .get(token)
                    .cloned()
                    .unwrap_or(token_type),
            );
        }
    }

    TokenizedResult {
        raw_value: raw_value.to_string(),
        tokens,
        types,
        classes,
    }
}

fn build_token_class_lookup(token_class_list: Option<&TokenClassList>) -> HashMap<String, String> {
    let mut temp_lookup: HashMap<String, Vec<String>> = HashMap::new();
    if let Some(class_list) = token_class_list {
        for (class_name, values) in class_list {
            for value in values {
                temp_lookup
                    .entry(value.clone())
                    .or_default()
                    .push(class_name.clone());
            }
        }
    }

    temp_lookup
        .into_iter()
        .map(|(value, classes)| (value, classes.join("|")))
        .collect()
}

fn compile_token_regex(pattern: &str) -> Pcre2Regex {
    Pcre2RegexBuilder::new()
        .utf(true)
        .ucp(true)
        .jit_if_available(true)
        .build(pattern)
        .expect("valid token regex")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_input_tokens_preserves_extended_boundaries() {
        assert_eq!(
            split_input_tokens("123 MAIN ST"),
            vec!["123", " ", "MAIN", " ", "ST"]
        );
        assert_eq!(
            split_input_tokens("APT-210 O'CONNOR"),
            vec!["APT-210", " ", "O'CONNOR"]
        );
        assert_eq!(
            split_input_tokens("WORD--ANOTHER...END"),
            vec!["WORD--ANOTHER", "...", "END"]
        );
    }

    #[test]
    fn test_get_token_fast_classifier_handles_common_shapes() {
        let names: HashSet<_> = vec![
            "NUM",
            "ALPHA",
            "POSTALCODE",
            "ALPHA_EXTENDED",
            "ALPHA_NUM_EXTENDED",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert_eq!(
            get_token_fast_classifier("123", &names),
            Some("NUM".to_string())
        );
        assert_eq!(
            get_token_fast_classifier("MAIN", &names),
            Some("ALPHA".to_string())
        );
        assert_eq!(
            get_token_fast_classifier("K1A0B1", &names),
            Some("POSTALCODE".to_string())
        );
        assert_eq!(
            get_token_fast_classifier("O'CONNOR", &names),
            Some("ALPHA_EXTENDED".to_string())
        );
        assert_eq!(
            get_token_fast_classifier("APT-210", &names),
            Some("ALPHA_NUM_EXTENDED".to_string())
        );
    }
}
