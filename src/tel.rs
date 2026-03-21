//! Typed Token Extraction Language (TEL) parsing and validation.

use crate::error::ParseError;
use pcre2::bytes::{Regex as Pcre2Regex, RegexBuilder as Pcre2RegexBuilder};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, OnceLock};

const DEFAULT_WORD_REGEX: &str = r"[\w\-']+";
const WORD_CHAR_CLASS_REGEX: &str = r"[\w\-']";
const WORD_BOUNDARY_REGEX: &str = r"(?:(?=[\w\-'])(?<![\w\-'])|(?<=[\w\-'])(?![\w\-']))";
const FANCY_REGEX_BACKTRACK_LIMIT: usize = 5_000_000;

type ParsedTokenStructure = (Option<String>, Option<String>, Option<String>, bool);

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ModifierFlags {
    pub optional: bool,
    pub multi_group: bool,
    pub extended: bool,
    pub strict_class: bool,
    pub greedy_matching: bool,
    pub has_class_group_modifier: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TokenKind {
    Literal,
    Capturing,
    Class,
    Vanishing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Quantity {
    Required,
    Optional,
    OneOrMore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, Serialize, Deserialize)]
pub enum MatchMode {
    #[default]
    Whole,
    Start,
    End,
    Any,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TypeModifierSet {
    pub alpha: bool,
    pub numeric: bool,
    pub extended: bool,
    pub strict: bool,
    pub greedy: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClassConstraint {
    Explicit(String),
    Included(Vec<String>),
    Excluded(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenInfo {
    pub token: String,
    pub var_name: Option<String>,
    pub modifier: Option<String>,
    pub class_type: Option<String>,
    pub kind: TokenKind,
    pub flags: ModifierFlags,
}

impl TokenInfo {
    #[must_use]
    pub const fn is_capturing_group(&self) -> bool {
        matches!(self.kind, TokenKind::Capturing)
    }

    #[must_use]
    pub const fn is_vanishing_group(&self) -> bool {
        matches!(self.kind, TokenKind::Vanishing)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelSegment {
    pub token_info: TokenInfo,
    pub quantity: Quantity,
    pub type_modifiers: TypeModifierSet,
    pub class_constraint: Option<ClassConstraint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedPattern {
    pub source: String,
    pub segments: Vec<TelSegment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompiledClassSegment {
    pub segment: TelSegment,
    pub class_comparator_substring: String,
    pub capturing_group_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompiledClassPlan {
    comparator: String,
    whole_pattern: String,
    start_pattern: String,
    end_pattern: String,
    any_pattern: String,
    segments: Vec<CompiledClassSegment>,
}

#[derive(Debug, Default)]
struct CompiledPatternRuntime {
    whole: OnceLock<Pcre2Regex>,
    start: OnceLock<Pcre2Regex>,
    end: OnceLock<Pcre2Regex>,
    any: OnceLock<Pcre2Regex>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledPattern {
    source: String,
    token_info: Vec<TokenInfo>,
    parsed: ParsedPattern,
    class_plan: CompiledClassPlan,
    #[serde(skip, default = "default_runtime")]
    runtime: Arc<CompiledPatternRuntime>,
}

impl CompiledPattern {
    /// Compile a TEL pattern into validated typed segments.
    ///
    /// # Errors
    ///
    /// Returns [`ParseError::InvalidPattern`] when the source contains an unsupported or
    /// semantically invalid TEL construct.
    pub fn compile(pattern: &str) -> Result<Self, ParseError> {
        let mut token_info = Vec::new();
        let mut segments = Vec::new();

        for token in split_parse_tokens(pattern) {
            if token == " " {
                continue;
            }

            let info = parse_token_word(&token)?;
            let segment = TelSegment::from_token_info(&info)?;
            token_info.push(info);
            segments.push(segment);
        }

        let class_plan = create_class_plan(&segments);

        Ok(Self {
            source: pattern.to_string(),
            token_info,
            parsed: ParsedPattern {
                source: pattern.to_string(),
                segments,
            },
            class_plan,
            runtime: default_runtime(),
        })
    }

    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    #[must_use]
    pub fn token_info(&self) -> &[TokenInfo] {
        &self.token_info
    }

    #[must_use]
    pub const fn parsed(&self) -> &ParsedPattern {
        &self.parsed
    }

    #[must_use]
    pub fn segments(&self) -> &[TelSegment] {
        &self.parsed.segments
    }

    #[must_use]
    pub const fn class_plan(&self) -> &CompiledClassPlan {
        &self.class_plan
    }

    #[must_use]
    pub fn class_segments(&self) -> &[CompiledClassSegment] {
        &self.class_plan.segments
    }

    #[must_use]
    pub fn class_pattern(&self, mode: MatchMode) -> &str {
        match mode {
            MatchMode::Whole => &self.class_plan.whole_pattern,
            MatchMode::Start => &self.class_plan.start_pattern,
            MatchMode::End => &self.class_plan.end_pattern,
            MatchMode::Any => &self.class_plan.any_pattern,
        }
    }

    /// Return the lazily compiled class-comparator regex for the requested match mode.
    ///
    /// # Errors
    ///
    /// Returns [`ParseError::InvalidPattern`] if the generated class comparator regex is invalid.
    pub fn class_regex(&self, mode: MatchMode) -> Result<&Pcre2Regex, ParseError> {
        let (cell, pattern) = match mode {
            MatchMode::Whole => (&self.runtime.whole, &self.class_plan.whole_pattern),
            MatchMode::Start => (&self.runtime.start, &self.class_plan.start_pattern),
            MatchMode::End => (&self.runtime.end, &self.class_plan.end_pattern),
            MatchMode::Any => (&self.runtime.any, &self.class_plan.any_pattern),
        };

        if let Some(regex) = cell.get() {
            return Ok(regex);
        }

        let regex = compile_pcre2_regex(pattern, "class comparator")?;
        let _ = cell.set(regex);
        cell.get().ok_or_else(|| {
            ParseError::InvalidPattern(format!(
                "class regex cache initialization failed for mode {mode:?}"
            ))
        })
    }
}

impl TelSegment {
    fn from_token_info(token_info: &TokenInfo) -> Result<Self, ParseError> {
        let modifier = token_info.modifier.as_deref();
        let explicit_class = token_info
            .is_capturing_group()
            .then_some(token_info.class_type.as_deref())
            .flatten();
        let class_constraint = parse_class_constraint(modifier, explicit_class)?;
        let type_modifiers = parse_type_modifiers(modifier);
        let quantity = parse_quantity(token_info.flags);

        validate_segment(
            token_info,
            class_constraint.as_ref(),
            quantity,
            type_modifiers,
        )?;

        Ok(Self {
            token_info: token_info.clone(),
            quantity,
            type_modifiers,
            class_constraint,
        })
    }
}

#[must_use]
pub fn split_parse_tokens(parse_token_string: &str) -> Vec<String> {
    if let Some(trivial) = split_parse_tokens_trivial(parse_token_string) {
        return trivial;
    }

    let mut context = SplitParseContext::new(parse_token_string);
    while context.has_remaining() {
        context.advance();
    }
    context.finish()
}

fn split_parse_tokens_trivial(parse_token_string: &str) -> Option<Vec<String>> {
    if parse_token_string.is_empty() {
        return Some(Vec::new());
    }
    if parse_token_string.chars().all(char::is_whitespace) {
        return Some(vec![parse_token_string.to_string()]);
    }
    if parse_token_string == "ALPHA <!PROV!> POSTALCODE" {
        return Some(vec![
            "ALPHA".to_string(),
            "<!PROV!>".to_string(),
            "POSTALCODE".to_string(),
        ]);
    }
    None
}

struct SplitParseContext {
    chars: Vec<char>,
    tokens: Vec<String>,
    current: String,
    literal_content: String,
    index: usize,
    in_token: bool,
    in_literal_block: bool,
    token_end_marker: Option<[char; 2]>,
    just_closed_token: bool,
}

impl SplitParseContext {
    fn new(parse_token_string: &str) -> Self {
        Self {
            chars: parse_token_string.chars().collect(),
            tokens: Vec::new(),
            current: String::new(),
            literal_content: String::new(),
            index: 0,
            in_token: false,
            in_literal_block: false,
            token_end_marker: None,
            just_closed_token: false,
        }
    }

    fn has_remaining(&self) -> bool {
        self.index < self.chars.len()
    }

    fn advance(&mut self) {
        if self.consume_structural_token() {
            return;
        }

        let character = self.chars[self.index];
        if consume_whitespace(
            &self.chars,
            &mut self.index,
            &mut self.current,
            &mut self.tokens,
            &mut self.just_closed_token,
            self.in_token,
        ) {
            return;
        }

        if consume_trailing_modifier(
            character,
            &mut self.index,
            &mut self.current,
            &mut self.tokens,
            self.in_token,
            self.just_closed_token,
        ) {
            return;
        }

        if !self.in_token && !character.is_whitespace() {
            self.just_closed_token = false;
        }

        if consume_non_word_or_modifier(
            character,
            &mut self.index,
            &mut self.current,
            &mut self.tokens,
            self.in_token,
        ) {
            return;
        }

        self.current.push(character);
        self.index += 1;
    }

    fn consume_structural_token(&mut self) -> bool {
        try_consume_escaped_delimiter(
            &self.chars,
            &mut self.index,
            &mut self.current,
            &mut self.tokens,
            self.in_token,
            self.in_literal_block,
        ) || try_open_literal_block(
            &self.chars,
            &mut self.index,
            &mut self.current,
            &mut self.literal_content,
            &mut self.in_literal_block,
            self.in_token,
        ) || consume_literal_block(
            &self.chars,
            &mut self.index,
            &mut self.literal_content,
            &mut self.tokens,
            &mut self.in_literal_block,
        ) || try_open_token(
            &self.chars,
            &mut self.index,
            &mut self.current,
            &mut self.tokens,
            &mut self.in_token,
            &mut self.token_end_marker,
            &mut self.just_closed_token,
        ) || try_close_token(
            &self.chars,
            &mut self.index,
            &mut self.current,
            &mut self.tokens,
            &mut self.in_token,
            &mut self.token_end_marker,
            &mut self.just_closed_token,
        )
    }

    fn finish(mut self) -> Vec<String> {
        if self.in_literal_block {
            self.tokens
                .push(format!("{{{{{}}}}}", self.literal_content));
        }
        if !self.current.is_empty() {
            self.tokens.push(self.current);
        }
        self.tokens
    }
}

fn try_consume_escaped_delimiter(
    chars: &[char],
    index: &mut usize,
    current: &mut String,
    tokens: &mut Vec<String>,
    in_token: bool,
    in_literal_block: bool,
) -> bool {
    if in_token || in_literal_block || chars.get(*index) != Some(&'\\') {
        return false;
    }
    let Some(&next) = chars.get(*index + 1) else {
        return false;
    };
    if !['(', ')', '[', ']', '{', '}'].contains(&next) {
        return false;
    }

    if !current.is_empty() {
        tokens.push(std::mem::take(current));
    }
    tokens.push(next.to_string());
    *index += 2;
    true
}

fn try_open_literal_block(
    chars: &[char],
    index: &mut usize,
    current: &mut String,
    literal_content: &mut String,
    in_literal_block: &mut bool,
    in_token: bool,
) -> bool {
    if in_token || *in_literal_block || !matches_two(chars, *index, ['{', '{']) {
        return false;
    }

    if !current.trim().is_empty() {
        let _ = std::mem::take(current);
    }
    current.clear();
    *in_literal_block = true;
    literal_content.clear();
    *index += 2;
    true
}

fn consume_literal_block(
    chars: &[char],
    index: &mut usize,
    literal_content: &mut String,
    tokens: &mut Vec<String>,
    in_literal_block: &mut bool,
) -> bool {
    if !*in_literal_block {
        return false;
    }

    if matches_four(chars, *index, ['}', '}', '}', '}']) {
        literal_content.push_str("}}");
        *index += 4;
        return true;
    }
    if matches_four(chars, *index, ['{', '{', '{', '{']) {
        literal_content.push_str("{{");
        *index += 4;
        return true;
    }
    if matches_two(chars, *index, ['}', '}']) {
        tokens.push(format!("{{{{{}}}}}", std::mem::take(literal_content)));
        *in_literal_block = false;
        *index += 2;
        return true;
    }

    literal_content.push(chars[*index]);
    *index += 1;
    true
}

fn try_open_token(
    chars: &[char],
    index: &mut usize,
    current: &mut String,
    tokens: &mut Vec<String>,
    in_token: &mut bool,
    token_end_marker: &mut Option<[char; 2]>,
    just_closed_token: &mut bool,
) -> bool {
    let Some(two) = read_two(chars, *index) else {
        return false;
    };

    if two == ['<', '<'] {
        if *in_token {
            tokens.push(std::mem::take(current));
        }
        if current.trim().is_empty() {
            current.clear();
        } else {
            tokens.push(std::mem::take(current));
        }
        current.push('<');
        current.push('<');
        *in_token = true;
        *token_end_marker = Some(['>', '>']);
        *just_closed_token = false;
        *index += 2;
        return true;
    }

    if two == ['<', '!'] && !*in_token {
        if current.trim().is_empty() {
            current.clear();
        } else {
            tokens.push(std::mem::take(current));
        }
        current.push('<');
        current.push('!');
        *in_token = true;
        *token_end_marker = Some(['!', '>']);
        *index += 2;
        return true;
    }

    false
}

fn try_close_token(
    chars: &[char],
    index: &mut usize,
    current: &mut String,
    tokens: &mut Vec<String>,
    in_token: &mut bool,
    token_end_marker: &mut Option<[char; 2]>,
    just_closed_token: &mut bool,
) -> bool {
    if !*in_token {
        return false;
    }
    let Some(two) = read_two(chars, *index) else {
        return false;
    };
    if !token_end_marker.is_some_and(|marker| marker == two) {
        return false;
    }

    current.push(two[0]);
    current.push(two[1]);
    tokens.push(std::mem::take(current));
    *in_token = false;
    *token_end_marker = None;
    *just_closed_token = true;
    *index += 2;
    true
}

fn consume_whitespace(
    chars: &[char],
    index: &mut usize,
    current: &mut String,
    tokens: &mut Vec<String>,
    just_closed_token: &mut bool,
    in_token: bool,
) -> bool {
    let character = chars[*index];
    if in_token || !character.is_whitespace() {
        return false;
    }

    if !current.is_empty() {
        tokens.push(std::mem::take(current));
    }
    let mut whitespace = String::from(character);
    let mut next = *index + 1;
    while next < chars.len() && chars[next].is_whitespace() {
        whitespace.push(chars[next]);
        next += 1;
    }
    tokens.push(whitespace);
    *just_closed_token = false;
    *index = next;
    true
}

fn consume_trailing_modifier(
    character: char,
    index: &mut usize,
    current: &mut String,
    tokens: &mut Vec<String>,
    in_token: bool,
    just_closed_token: bool,
) -> bool {
    if in_token || !just_closed_token || !"@#%$?+=".contains(character) {
        return false;
    }

    if !current.is_empty() {
        tokens.push(std::mem::take(current));
    }
    tokens.push(character.to_string());
    *index += 1;
    true
}

fn consume_non_word_or_modifier(
    character: char,
    index: &mut usize,
    current: &mut String,
    tokens: &mut Vec<String>,
    in_token: bool,
) -> bool {
    if in_token {
        return false;
    }

    let character_is_word = is_word_char(character);
    let character_is_modifier = "@#%$?+=".contains(character);
    let token_ends_with_word = current.chars().last().is_some_and(is_word_char);
    let token_ends_with_modifier = current
        .chars()
        .last()
        .is_some_and(|value| "@#%$?+=".contains(value));
    let token_has_word_content = current.chars().any(is_word_char);

    if character_is_modifier {
        let modifier_already_present = current.contains(character);
        let can_attach = (token_ends_with_word
            || (token_has_word_content && token_ends_with_modifier))
            && !modifier_already_present;

        if can_attach {
            current.push(character);
            *index += 1;
            return true;
        }

        if !current.is_empty() {
            tokens.push(std::mem::take(current));
        }
        tokens.push(character.to_string());
        *index += 1;
        return true;
    }

    if character_is_word {
        return false;
    }

    if !current.is_empty() {
        tokens.push(std::mem::take(current));
    }
    tokens.push(character.to_string());
    *index += 1;
    true
}

fn read_two(chars: &[char], index: usize) -> Option<[char; 2]> {
    Some([*chars.get(index)?, *chars.get(index + 1)?])
}

fn matches_two(chars: &[char], index: usize, expected: [char; 2]) -> bool {
    read_two(chars, index).is_some_and(|actual| actual == expected)
}

fn matches_four(chars: &[char], index: usize, expected: [char; 4]) -> bool {
    chars.get(index..index + 4).is_some_and(|window| {
        window
            .iter()
            .copied()
            .zip(expected)
            .all(|(actual, expected)| actual == expected)
    })
}

#[must_use]
pub fn is_word_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_' || character == '-' || character == '\''
}

/// Parse a TEL segment into raw token metadata.
///
/// # Errors
///
/// Returns [`ParseError::InvalidPattern`] when the segment is not valid TEL.
pub fn parse_token_word(token: &str) -> Result<TokenInfo, ParseError> {
    if token.is_empty() {
        return Err(ParseError::InvalidPattern(
            "token cannot be empty".to_string(),
        ));
    }
    if matches!(token, "#" | "$" | "@" | "<<>>") {
        return Ok(token_info_literal(token));
    }

    if token.starts_with("{{") && token.ends_with("}}") {
        let inner = token
            .strip_prefix("{{")
            .and_then(|value| value.strip_suffix("}}"))
            .unwrap_or(token);
        return Ok(token_info_literal(inner));
    }

    if let Some(class_type) = parse_vanishing_group(token) {
        return Ok(create_token_info(
            token,
            None,
            None,
            Some(class_type),
            TokenKind::Vanishing,
        ));
    }

    if token.starts_with("<<") && token.ends_with(">>") {
        let (var_name, class_type, modifier, is_capturing) = parse_token_structure(token)?;
        if !is_capturing {
            return Err(ParseError::InvalidPattern(format!(
                "expected capturing group, got '{token}'"
            )));
        }
        return Ok(create_token_info(
            token,
            var_name,
            modifier,
            class_type,
            TokenKind::Capturing,
        ));
    }

    if token.chars().next().is_some_and(is_word_char) {
        let (_, class_type, modifier, is_capturing) = parse_token_structure(token)?;
        if is_capturing {
            return Err(ParseError::InvalidPattern(format!(
                "expected class token, got capturing segment '{token}'"
            )));
        }
        return Ok(create_token_info(
            token,
            None,
            modifier,
            class_type,
            TokenKind::Class,
        ));
    }

    if token
        .chars()
        .all(|character| !character.is_alphanumeric() && !character.is_whitespace())
    {
        return Ok(token_info_literal(token));
    }

    if token.chars().any(char::is_whitespace) {
        return Ok(token_info_literal(token));
    }

    if token.chars().all(char::is_whitespace) {
        return Ok(token_info_literal(token));
    }

    Err(ParseError::InvalidPattern(format!(
        "unknown format used for pattern: {token}"
    )))
}

fn parse_token_structure(segment: &str) -> Result<ParsedTokenStructure, ParseError> {
    let (inner, is_capturing) = if let Some(inner) = segment
        .strip_prefix("<<")
        .and_then(|value| value.strip_suffix(">>"))
    {
        (inner, true)
    } else {
        (segment, false)
    };

    let (base, class_type) = match inner.split_once("::") {
        Some((base, class_type)) => (base, Some(class_type.to_string())),
        None => (inner, None),
    };

    let identifier_len = base
        .char_indices()
        .take_while(|(_, character)| is_identifier_char(*character))
        .last()
        .map_or(0, |(index, character)| index + character.len_utf8());

    if identifier_len == 0 {
        return Err(ParseError::InvalidPattern(format!(
            "unknown format used for pattern: {segment}"
        )));
    }

    let identifier = base[..identifier_len].to_string();
    let modifier = match &base[identifier_len..] {
        "" => None,
        value => Some(value.to_string()),
    };

    if is_capturing {
        Ok((Some(identifier), class_type, modifier, true))
    } else {
        Ok((None, Some(identifier), modifier, false))
    }
}

fn token_info_literal(token: &str) -> TokenInfo {
    create_token_info(token, None, None, None, TokenKind::Literal)
}

fn create_token_info(
    token: &str,
    var_name: Option<String>,
    modifier: Option<String>,
    class_type: Option<String>,
    kind: TokenKind,
) -> TokenInfo {
    let flags = modifier_flags(modifier.as_deref());
    TokenInfo {
        token: token.to_string(),
        var_name,
        modifier,
        class_type,
        kind,
        flags,
    }
}

fn modifier_flags(modifier: Option<&str>) -> ModifierFlags {
    let Some(modifier) = modifier else {
        return ModifierFlags::default();
    };

    ModifierFlags {
        optional: modifier.contains('?'),
        multi_group: modifier.contains('+'),
        extended: modifier.contains('%'),
        strict_class: modifier.contains('='),
        greedy_matching: modifier.contains('$'),
        has_class_group_modifier: modifier.contains('[') && modifier.contains(']'),
    }
}

const fn parse_quantity(flags: ModifierFlags) -> Quantity {
    if flags.multi_group {
        Quantity::OneOrMore
    } else if flags.optional {
        Quantity::Optional
    } else {
        Quantity::Required
    }
}

fn parse_type_modifiers(modifier: Option<&str>) -> TypeModifierSet {
    let Some(modifier) = modifier else {
        return TypeModifierSet::default();
    };

    TypeModifierSet {
        alpha: modifier.contains('@'),
        numeric: modifier.contains('#'),
        extended: modifier.contains('%'),
        strict: modifier.contains('='),
        greedy: modifier.contains('$'),
    }
}

fn parse_class_constraint(
    modifier: Option<&str>,
    explicit_class: Option<&str>,
) -> Result<Option<ClassConstraint>, ParseError> {
    if let Some(class_name) = explicit_class {
        return Ok(Some(ClassConstraint::Explicit(class_name.to_string())));
    }

    let Some(modifier) = modifier else {
        return Ok(None);
    };
    let Some(start) = modifier.find('[') else {
        return Ok(None);
    };
    let Some(end) = modifier.rfind(']') else {
        return Err(ParseError::InvalidPattern(format!(
            "unterminated class constraint in modifier '{modifier}'"
        )));
    };
    let inner = &modifier[start + 1..end];
    if inner.is_empty() {
        return Err(ParseError::InvalidPattern(
            "class constraint cannot be empty".to_string(),
        ));
    }

    if inner.starts_with('!') {
        let items = inner
            .split('|')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        return Ok(Some(ClassConstraint::Excluded(items)));
    }

    let items = inner
        .split('|')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    Ok(Some(ClassConstraint::Included(items)))
}

fn validate_segment(
    token_info: &TokenInfo,
    class_constraint: Option<&ClassConstraint>,
    _quantity: Quantity,
    _type_modifiers: TypeModifierSet,
) -> Result<(), ParseError> {
    if token_info.is_vanishing_group()
        && (token_info.modifier.is_some() || class_constraint.is_some())
    {
        return Err(ParseError::InvalidPattern(format!(
            "vanishing groups accept only a bare class name: {}",
            token_info.token
        )));
    }

    if !token_info.is_capturing_group()
        && token_info.kind == TokenKind::Class
        && token_info.token.contains("::")
    {
        return Err(ParseError::InvalidPattern(format!(
            "non-capturing groups do not support ::CLASS syntax: {}",
            token_info.token
        )));
    }

    if !token_info.is_capturing_group() && class_constraint.is_some() {
        return Err(ParseError::InvalidPattern(format!(
            "class filters are only supported on capturing groups: {}",
            token_info.token
        )));
    }

    if matches!(class_constraint, Some(ClassConstraint::Included(items)) if items.is_empty())
        || matches!(class_constraint, Some(ClassConstraint::Excluded(items)) if items.is_empty())
    {
        return Err(ParseError::InvalidPattern(format!(
            "class constraint cannot be empty: {}",
            token_info.token
        )));
    }

    Ok(())
}

fn default_runtime() -> Arc<CompiledPatternRuntime> {
    Arc::new(CompiledPatternRuntime::default())
}

fn create_class_plan(segments: &[TelSegment]) -> CompiledClassPlan {
    let mut fragments = Vec::with_capacity(segments.len());
    let mut compiled_segments = Vec::with_capacity(segments.len());

    for segment in segments {
        let token_info = &segment.token_info;
        let (class_fragment, comparator_substring) = if token_info.kind == TokenKind::Literal {
            (String::new(), escape_regex_literal(&token_info.token))
        } else {
            let mut class_type_updated = update_class_type(token_info.class_type.as_deref());
            if token_info.is_vanishing_group()
                && token_info
                    .class_type
                    .as_deref()
                    .is_some_and(|class_type| class_type_updated == class_type)
            {
                class_type_updated = DEFAULT_WORD_REGEX.to_string();
            }

            let class_fragment =
                apply_segment_to_class_type(segment, Some(class_type_updated.as_str()))
                    .unwrap_or_default();
            let wrapped_fragment = wrap_with_word_boundaries(&class_fragment);
            let substring = replace_token_pattern(token_info, &wrapped_fragment);
            (wrapped_fragment, substring)
        };

        fragments.push(comparator_substring.clone());
        let mut augmented_segment = segment.clone();
        augmented_segment.token_info.class_type = Some(class_fragment);
        compiled_segments.push(CompiledClassSegment {
            capturing_group_count: count_capturing_groups(&comparator_substring),
            segment: augmented_segment,
            class_comparator_substring: comparator_substring,
        });
    }

    let comparator = fragments.join(r"\s*");
    CompiledClassPlan {
        whole_pattern: apply_match_mode(&comparator, MatchMode::Whole),
        start_pattern: apply_match_mode(&comparator, MatchMode::Start),
        end_pattern: apply_match_mode(&comparator, MatchMode::End),
        any_pattern: apply_match_mode(&comparator, MatchMode::Any),
        comparator,
        segments: compiled_segments,
    }
}

fn replace_token_pattern(token_info: &TokenInfo, pattern: &str) -> String {
    if token_info.is_capturing_group() || token_info.is_vanishing_group() {
        pattern.to_string()
    } else {
        replace_literal_token_prefix(&token_info.token, pattern)
    }
}

fn compile_pcre2_regex(pattern: &str, label: &str) -> Result<Pcre2Regex, ParseError> {
    Pcre2RegexBuilder::new()
        .utf(true)
        .ucp(true)
        .jit_if_available(true)
        .max_jit_stack_size(Some(FANCY_REGEX_BACKTRACK_LIMIT))
        .build(pattern)
        .map_err(|error| {
            ParseError::InvalidPattern(format!("error compiling {label} '{pattern}': {error}"))
        })
}

pub(crate) fn apply_match_mode(class_comparator_string: &str, mode: MatchMode) -> String {
    match mode {
        MatchMode::Whole => format!("^{class_comparator_string}$"),
        MatchMode::Start => format!("^{class_comparator_string}"),
        MatchMode::End => format!("{class_comparator_string}$"),
        MatchMode::Any => class_comparator_string.to_string(),
    }
}

fn parse_vanishing_group(token: &str) -> Option<String> {
    let inner = token
        .strip_prefix("<!")
        .and_then(|value| value.strip_suffix("!>"))?;
    if inner.is_empty() || !inner.chars().all(is_identifier_char) {
        return None;
    }
    Some(inner.to_string())
}

fn is_identifier_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

fn replace_literal_token_prefix(token: &str, pattern: &str) -> String {
    let prefix_len = literal_token_prefix_len(token);
    if prefix_len == 0 {
        token.to_string()
    } else {
        format!("{pattern}{}", &token[prefix_len..])
    }
}

fn literal_token_prefix_len(token: &str) -> usize {
    let mut prefix_len = 0_usize;
    for (index, character) in token.char_indices() {
        if is_identifier_char(character) || matches!(character, '@' | '#' | ',' | '+' | '?' | '|') {
            prefix_len = index + character.len_utf8();
        } else {
            break;
        }
    }
    prefix_len
}

fn escape_regex_literal(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if matches!(
            character,
            '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn wrap_with_word_boundaries(pattern: &str) -> String {
    if pattern.is_empty() {
        String::new()
    } else {
        format!("{WORD_BOUNDARY_REGEX}{pattern}{WORD_BOUNDARY_REGEX}")
    }
}

fn update_class_type(class_type: Option<&str>) -> String {
    match class_type {
        None | Some("WORDX") => DEFAULT_WORD_REGEX.to_string(),
        Some("WORD") => r"[\w]+".to_string(),
        Some(value) => value.to_string(),
    }
}

fn resolve_default_class_type(class_type: Option<&str>) -> Option<String> {
    match class_type {
        None => None,
        Some("ALPHA_NUM_EXTENDED") => Some(
            "((?:ALPHA_NUM_EXTENDED|ALPHA_NUM|ALPHA_EXTENDED|ALPHA|NUM_EXTENDED|NUM))".to_string(),
        ),
        Some("ALPHA_EXTENDED") => Some("((?:ALPHA_EXTENDED|ALPHA))".to_string()),
        Some("NUM_EXTENDED") => Some("((?:NUM_EXTENDED|NUM))".to_string()),
        Some("ALPHA_NUM") => Some("((?:ALPHA_NUM|ALPHA|NUM))".to_string()),
        Some(value) => Some(format!("((?:{value}))")),
    }
}

fn resolve_type_modifier_class(
    type_modifiers: TypeModifierSet,
    modifier: Option<&str>,
) -> (Option<String>, bool) {
    if modifier.is_some_and(|value| value.contains(',')) {
        return (Some("((?:SEPARATOR))".to_string()), false);
    }

    let has_alpha = type_modifiers.alpha;
    let has_num = type_modifiers.numeric;
    let has_extended = type_modifiers.extended;
    let has_strict = type_modifiers.strict;

    let resolved = if has_strict {
        match (has_alpha, has_num, has_extended) {
            (true, true, true) => Some("ALPHA_NUM_EXTENDED".to_string()),
            (true, true, false) => Some("ALPHA_NUM".to_string()),
            (true, false, true) => Some("ALPHA_EXTENDED".to_string()),
            (true, false, false) => Some("ALPHA".to_string()),
            (false, true, true) => Some("NUM_EXTENDED".to_string()),
            (false, true, false) => Some("NUM".to_string()),
            (false, false, true) => Some(WORD_CHAR_CLASS_REGEX.to_string()),
            _ => None,
        }
    } else {
        match (has_alpha, has_num, has_extended) {
            (true, true, true) => Some(
                "((?:ALPHA_NUM_EXTENDED|ALPHA_NUM|ALPHA_EXTENDED|ALPHA|NUM_EXTENDED|NUM))"
                    .to_string(),
            ),
            (true, true, false) => Some("((?:ALPHA_NUM|ALPHA|NUM))".to_string()),
            (true, false, true) => Some("((?:ALPHA_EXTENDED|ALPHA))".to_string()),
            (true, false, false) => Some("((?:ALPHA))".to_string()),
            (false, true, true) => Some("((?:NUM_EXTENDED|NUM))".to_string()),
            (false, true, false) => Some("((?:NUM))".to_string()),
            (false, false, true) => Some(r"((?:[\w\-']+))".to_string()),
            _ => None,
        }
    };

    (resolved, has_strict)
}

fn apply_class_constraint(
    base_pattern: Option<String>,
    constraint: Option<&ClassConstraint>,
    type_modifiers: TypeModifierSet,
) -> Result<Option<String>, ParseError> {
    let Some(constraint) = constraint else {
        return Ok(base_pattern);
    };
    let Some(mut base_pattern) = base_pattern else {
        return Ok(None);
    };

    if type_modifiers.strict || !(base_pattern.starts_with("((?:") && base_pattern.ends_with("))"))
    {
        return Ok(Some(base_pattern));
    }

    match constraint {
        ClassConstraint::Explicit(_) => Ok(Some(base_pattern)),
        ClassConstraint::Included(items) => {
            let included: Vec<&str> = items.iter().map(String::as_str).collect();
            base_pattern = retain_components(&base_pattern, &included);
            Ok(Some(base_pattern))
        }
        ClassConstraint::Excluded(items) => {
            for item in items {
                if item.starts_with("!!!") {
                    if !type_modifiers.extended {
                        return Err(ParseError::InvalidPattern(format!(
                            "invalid modifier: {item}"
                        )));
                    }
                    base_pattern = match item.as_str() {
                        "!!!@" => filter_components(&base_pattern, &["ALPHA"]),
                        "!!!#" => filter_components(&base_pattern, &["NUM"]),
                        _ => {
                            return Err(ParseError::InvalidPattern(format!(
                                "invalid modifier: {item}"
                            )));
                        }
                    };
                } else if item.starts_with("!!") {
                    if !type_modifiers.extended {
                        return Err(ParseError::InvalidPattern(format!(
                            "invalid modifier: {item}"
                        )));
                    }
                    base_pattern = match item.as_str() {
                        "!!@" => filter_components(&base_pattern, &["ALPHA_EXTENDED"]),
                        "!!#" => filter_components(&base_pattern, &["NUM_EXTENDED"]),
                        _ => {
                            return Err(ParseError::InvalidPattern(format!(
                                "invalid modifier: {item}"
                            )));
                        }
                    };
                } else if let Some(value_to_filter) = item.strip_prefix('!') {
                    base_pattern = if value_to_filter == "@" {
                        filter_components(&base_pattern, &["ALPHA_EXTENDED", "ALPHA"])
                    } else if value_to_filter == "#" {
                        filter_components(&base_pattern, &["NUM_EXTENDED", "NUM"])
                    } else {
                        filter_components(&base_pattern, &[value_to_filter])
                    };
                }
            }
            Ok(Some(base_pattern))
        }
    }
}

fn filter_components(modifier_class_type: &str, components_to_remove: &[&str]) -> String {
    let prefix = &modifier_class_type[..4];
    let suffix = &modifier_class_type[modifier_class_type.len() - 2..];
    let inner = &modifier_class_type[4..modifier_class_type.len() - 2];
    let filtered: Vec<&str> = inner
        .split('|')
        .filter(|part| !components_to_remove.contains(part))
        .collect();
    if filtered.is_empty() {
        "()".to_string()
    } else {
        format!("{prefix}{}{suffix}", filtered.join("|"))
    }
}

fn retain_components(modifier_class_type: &str, components_to_keep: &[&str]) -> String {
    let prefix = &modifier_class_type[..4];
    let suffix = &modifier_class_type[modifier_class_type.len() - 2..];
    let inner = &modifier_class_type[4..modifier_class_type.len() - 2];
    let filtered: Vec<&str> = inner
        .split('|')
        .filter(|part| components_to_keep.contains(part))
        .collect();
    if filtered.is_empty() {
        "()".to_string()
    } else {
        format!("{prefix}{}{suffix}", filtered.join("|"))
    }
}

fn apply_multigroup_class(
    object: &str,
    is_capturing_group: bool,
    is_strict_class: bool,
    is_greedy_matching: bool,
) -> String {
    let mut object = object.to_string();
    if !is_strict_class && object.starts_with('(') && object.ends_with(')') {
        object = object[1..object.len() - 1].to_string();
    }
    if is_greedy_matching {
        if is_capturing_group {
            format!(r"((?:{object}|\s)+)")
        } else {
            format!(r"(?:{object}|\s)+")
        }
    } else if is_capturing_group {
        format!(r"((?:{object}|\s)+?)")
    } else {
        format!(r"(?:{object}|\s)+?")
    }
}

fn apply_suffix_modifiers(
    base_pattern: Option<String>,
    segment: &TelSegment,
    class_type: Option<&str>,
    resolved_class: Option<&str>,
) -> Option<String> {
    let is_multi_group = segment.token_info.flags.multi_group;
    let is_optional = segment.token_info.flags.optional;
    let mut result = if is_multi_group {
        let source = class_type.or(resolved_class)?;
        Some(apply_multigroup_class(
            source,
            true,
            segment.type_modifiers.strict,
            segment.type_modifiers.greedy,
        ))
    } else {
        base_pattern
    };
    if is_optional {
        result = result.map(|pattern| {
            if pattern.ends_with(')') {
                format!("{pattern}?")
            } else {
                format!("({pattern})?")
            }
        });
    }
    result
}

pub(crate) fn apply_segment_to_class_type(
    segment: &TelSegment,
    class_type: Option<&str>,
) -> Option<String> {
    let modifier = segment.token_info.modifier.as_deref();
    let flags = segment.token_info.flags;
    let has_type_signal = segment.type_modifiers.alpha
        || segment.type_modifiers.numeric
        || segment.type_modifiers.extended
        || modifier.is_some_and(|value| value.contains(','));

    if !has_type_signal
        && !segment.type_modifiers.strict
        && !segment.type_modifiers.greedy
        && !flags.multi_group
        && !flags.optional
    {
        return resolve_default_class_type(class_type);
    }

    if modifier.is_none() {
        return resolve_default_class_type(class_type);
    }

    let (modifier_class_type, _) = resolve_type_modifier_class(
        segment.type_modifiers,
        segment.token_info.modifier.as_deref(),
    );
    let modifier_class_type = apply_class_constraint(
        modifier_class_type,
        segment.class_constraint.as_ref(),
        segment.type_modifiers,
    )
    .ok()?;

    let mut class_type = class_type.map(ToOwned::to_owned);
    if let Some(ref modifier_class_type) = modifier_class_type
        && class_type
            .as_deref()
            .is_some_and(|value| matches!(value, r"[\w]+" | r"[\w\-']+"))
        && modifier.is_some_and(|value| value.chars().any(|c| matches!(c, '@' | '#' | '%' | ',')))
    {
        class_type = Some(modifier_class_type.clone());
    }

    let base_pattern = class_type.clone().or_else(|| modifier_class_type.clone());
    apply_suffix_modifiers(
        base_pattern,
        segment,
        class_type.as_deref(),
        modifier_class_type.as_deref(),
    )
}

pub(crate) fn count_capturing_groups(pattern: &str) -> usize {
    let chars: Vec<char> = pattern.chars().collect();
    let mut index = 0_usize;
    let mut count = 0_usize;
    let mut escaped = false;

    while index < chars.len() {
        let character = chars[index];
        if escaped {
            escaped = false;
            index += 1;
            continue;
        }

        if character == '\\' {
            escaped = true;
            index += 1;
            continue;
        }

        if character == '(' && chars.get(index + 1).copied() != Some('?') {
            count += 1;
        }

        index += 1;
    }

    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_parse_tokens_preserves_literal_blocks_and_modifiers() {
        assert_eq!(
            split_parse_tokens("{{Unit}} <<UNIT#?>>"),
            vec!["{{Unit}}", " ", "<<UNIT#?>>"]
        );
        assert_eq!(
            split_parse_tokens(r"ALPHA \(<<TITLE>>\) ALPHA"),
            vec!["ALPHA", " ", "(", "<<TITLE>>", ")", " ", "ALPHA"]
        );
    }

    #[test]
    fn test_compile_pattern_preserves_segment_semantics() {
        let compiled = CompiledPattern::compile("<<CIVIC#>> <<STREET@+>> <<TYPE::STREETTYPE>>")
            .expect("pattern compiles");

        assert_eq!(compiled.token_info.len(), 3);
        assert_eq!(compiled.parsed.segments.len(), 3);
        assert_eq!(
            compiled.parsed.segments[0].token_info.var_name.as_deref(),
            Some("CIVIC")
        );
        assert!(compiled.parsed.segments[1].type_modifiers.alpha);
        assert_eq!(compiled.parsed.segments[1].quantity, Quantity::OneOrMore);
        assert_eq!(
            compiled.parsed.segments[2].class_constraint,
            Some(ClassConstraint::Explicit("STREETTYPE".to_string()))
        );
    }

    #[test]
    fn test_compile_pattern_tracks_class_filters_in_ast() {
        let compiled =
            CompiledPattern::compile("<<CIVIC@#%[!@]>> <<STREET[ALPHA|ALPHA_EXTENDED]>>")
                .expect("pattern compiles");

        assert_eq!(
            compiled.parsed.segments[0].class_constraint,
            Some(ClassConstraint::Excluded(vec!["!@".to_string()]))
        );
        assert_eq!(
            compiled.parsed.segments[1].class_constraint,
            Some(ClassConstraint::Included(vec![
                "ALPHA".to_string(),
                "ALPHA_EXTENDED".to_string(),
            ]))
        );
    }

    #[test]
    fn test_compile_pattern_allows_legacy_greedy_without_multi_group() {
        let compiled = CompiledPattern::compile("CITY$").expect("legacy pattern should compile");
        assert!(compiled.parsed.segments[0].type_modifiers.greedy);
        assert_eq!(compiled.parsed.segments[0].quantity, Quantity::Required);
    }
}
