use unicode_normalization::UnicodeNormalization;

#[must_use]
pub fn to_upper(s: &str) -> String {
    s.to_uppercase()
}

#[must_use]
pub fn strip_accents(text: &str) -> String {
    use unicode_general_category::{GeneralCategory, get_general_category};
    text.nfkd()
        .filter(|ch| {
            let cat = get_general_category(*ch);
            !matches!(
                cat,
                GeneralCategory::NonspacingMark
                    | GeneralCategory::SpacingMark
                    | GeneralCategory::EnclosingMark
            )
        })
        .collect()
}

#[must_use]
pub fn normalize_value(value: &str) -> String {
    let upper = value.trim().to_uppercase();
    upper.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[must_use]
pub fn normalize_loose(value: &str) -> String {
    let normalized = normalize_value(value);
    let stripped = strip_accents(&normalized);
    let with_spaces: String = stripped
        .chars()
        .map(|character| {
            if character.is_ascii_uppercase() || character.is_ascii_digit() {
                character
            } else {
                ' '
            }
        })
        .collect();
    with_spaces.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[must_use]
pub fn tokenize_loose(value: &str) -> Vec<String> {
    let normalized = normalize_loose(value);
    normalized
        .split_whitespace()
        .map(std::string::ToString::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_value() {
        assert_eq!(normalize_value("  Main   St  "), "MAIN ST");
        assert_eq!(normalize_value("ottawa"), "OTTAWA");
    }

    #[test]
    fn test_strip_accents() {
        assert_eq!(strip_accents("Sainte-Thérèse"), "Sainte-Therese");
        assert_eq!(strip_accents("Riviere-du-Loup"), "Riviere-du-Loup");
        assert_eq!(strip_accents("L'Ile-Bizard"), "L'Ile-Bizard");
    }

    #[test]
    fn test_normalize_loose() {
        assert_eq!(normalize_loose("Sainte-Thérèse"), "SAINTE THERESE");
        assert_eq!(normalize_loose("RUE DE L'EGLISE"), "RUE DE L EGLISE");
    }

    #[test]
    fn test_tokenize_loose() {
        assert_eq!(tokenize_loose("Sainte-Thérèse"), vec!["SAINTE", "THERESE"]);
        assert_eq!(tokenize_loose("123 Main St."), vec!["123", "MAIN", "ST"]);
    }
}
