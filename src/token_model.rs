//! Typed token model loading and compiled lookup state.

use pcre2::bytes::{Regex as Pcre2Regex, RegexBuilder as Pcre2RegexBuilder};
use std::collections::{HashMap, HashSet};
use std::path::Path;

pub type TokenDefinition = Vec<(String, String)>;
pub type TokenClassList = Vec<(String, HashSet<String>)>;

#[derive(Debug)]
pub struct TokenModel {
    token_definitions: TokenDefinition,
    token_class_list: TokenClassList,
    compiled_patterns: Vec<(String, Pcre2Regex)>,
    available_names: HashSet<String>,
    token_class_lookup: HashMap<String, String>,
}

impl TokenModel {
    /// Build a compiled token model from loaded definitions and classes.
    ///
    /// # Panics
    ///
    /// Panics if a token definition regex is invalid after start/end anchors are applied.
    #[must_use]
    pub fn from_parts(
        token_definitions: TokenDefinition,
        token_class_list: TokenClassList,
    ) -> Self {
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
                (name.clone(), compile_token_pattern(&anchored))
            })
            .collect();

        Self {
            token_class_lookup: build_token_class_lookup(Some(&token_class_list)),
            token_definitions,
            token_class_list,
            compiled_patterns,
            available_names,
        }
    }

    /// Load the default wanParser model layout from a base directory.
    ///
    /// Expected layout:
    /// - `TOKENDEFINITION/TOKENDEFINITONS.param2`
    /// - `TOKENCLASS/*.param`
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the model files cannot be read.
    pub fn load<P: AsRef<Path>>(base_path: P) -> std::io::Result<Self> {
        let base_path = base_path.as_ref();
        let token_definitions =
            load_token_definitions(base_path.join("TOKENDEFINITION/TOKENDEFINITONS.param2"))?;
        let token_class_list = load_token_class_list(base_path.join("TOKENCLASS"))?;
        Ok(Self::from_parts(token_definitions, token_class_list))
    }

    #[must_use]
    pub const fn token_definitions(&self) -> &TokenDefinition {
        &self.token_definitions
    }

    #[must_use]
    pub const fn token_class_list(&self) -> &TokenClassList {
        &self.token_class_list
    }

    #[must_use]
    pub fn compiled_patterns(&self) -> &[(String, Pcre2Regex)] {
        &self.compiled_patterns
    }

    #[must_use]
    pub const fn available_names(&self) -> &HashSet<String> {
        &self.available_names
    }

    #[must_use]
    pub const fn token_class_lookup(&self) -> &HashMap<String, String> {
        &self.token_class_lookup
    }
}

/// Load `.param2` token definitions from disk.
///
/// # Errors
///
/// Returns an I/O error when the file cannot be read.
pub fn load_token_definitions<P: AsRef<Path>>(path: P) -> std::io::Result<TokenDefinition> {
    let content = std::fs::read_to_string(path)?;
    let mut definitions = Vec::new();

    for line in content.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let name = extract_tag_value(line, "NAME");
        let value = extract_tag_value(line, "VALUE");

        if let (Some(name), Some(value)) = (name, value) {
            definitions.push((name, value));
        }
    }

    Ok(definitions)
}

/// Load token class lists from the `TOKENCLASS` directory.
///
/// # Errors
///
/// Returns an I/O error when the directory cannot be read.
pub fn load_token_class_list<P: AsRef<Path>>(dir_path: P) -> std::io::Result<TokenClassList> {
    let mut class_list = Vec::new();
    if !dir_path.as_ref().exists() {
        return Ok(class_list);
    }

    for entry in std::fs::read_dir(dir_path)? {
        let entry = entry?;
        let path = entry.path();
        if !(path.is_file() && path.extension().and_then(|value| value.to_str()) == Some("param")) {
            continue;
        }

        let class_name = path
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid class name")
            })?
            .to_string();
        let content = std::fs::read_to_string(&path)?;
        let values: HashSet<String> = content
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(ToString::to_string)
            .collect();
        class_list.push((class_name, values));
    }

    Ok(class_list)
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

fn extract_tag_value(line: &str, tag: &str) -> Option<String> {
    let opening = format!("<{tag}>");
    let closing = format!("</{tag}>");
    let start = line.find(&opening)? + opening.len();
    let end = line[start..].find(&closing)? + start;
    Some(line[start..end].to_string())
}

fn compile_token_pattern(pattern: &str) -> Pcre2Regex {
    Pcre2RegexBuilder::new()
        .utf(true)
        .ucp(true)
        .jit_if_available(true)
        .build(pattern)
        .expect("valid token definition regex")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_token_model_loads_fixture_model() {
        let base_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/model_1");

        let model = TokenModel::load(base_path).expect("fixture model should load");
        assert!(!model.token_definitions().is_empty());
        assert!(!model.available_names().is_empty());
        assert!(!model.compiled_patterns().is_empty());
    }
}
