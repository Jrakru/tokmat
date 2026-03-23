//! Extraction engine ported from the Python wanParser implementation.

use crate::error::ParseError;
use crate::tel::{
    CompiledClassSegment, CompiledPattern, Quantity, TelSegment, TokenInfo, apply_match_mode,
    apply_segment_to_class_type,
};
use crate::tokenizer::{
    TokenClassList, TokenDefinition, split_input_tokens, tokenize_and_classify,
};
use lru::LruCache;
use pcre2::bytes::{
    Captures as Pcre2Captures, Regex as Pcre2Regex, RegexBuilder as Pcre2RegexBuilder,
};
use std::collections::HashMap;
use std::hash::Hash;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const DEFAULT_WORD_REGEX: &str = r"[\w\-']+";
const WORD_BOUNDARY_REGEX: &str = r"(?:(?=[\w\-'])(?<![\w\-'])|(?<=[\w\-'])(?![\w\-']))";
const FANCY_REGEX_BACKTRACK_LIMIT: usize = 5_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjComparatorTokenInfo {
    pub segment: TelSegment,
    pub class_comparator_substring: String,
    pub multi_group_optional: Option<String>,
    pub new_class_type: Option<String>,
    pub regex_pattern: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseOutput {
    pub uid: String,
    pub fields: HashMap<String, String>,
    pub complement: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtractorConfig {
    pub compiled_pattern_cache_capacity: usize,
    pub object_plan_cache_capacity: usize,
    pub fallback_regex_cache_capacity: usize,
}

impl Default for ExtractorConfig {
    fn default() -> Self {
        Self {
            // Sized to cover the observed working set of the imported upstream
            // wanParser corpus without pathological eviction churn.
            compiled_pattern_cache_capacity: 512,
            object_plan_cache_capacity: 2048,
            fallback_regex_cache_capacity: 2048,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CacheStats {
    pub capacity: usize,
    pub len: usize,
    pub hits: usize,
    pub misses: usize,
    pub inserts: usize,
    pub evictions: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExtractorStats {
    pub compiled_pattern_cache: CacheStats,
    pub object_plan_cache: CacheStats,
    pub fallback_regex_cache: CacheStats,
    pub unique_plan_signature_count: usize,
    pub direct_only_plan_count: usize,
    pub with_fallback_plan_count: usize,
    pub total_plan_steps: usize,
    pub single_token_step_count: usize,
    pub captured_span_step_count: usize,
    pub literal_step_count: usize,
    pub direct_execution_attempts: usize,
    pub direct_execution_hits: usize,
    pub fallback_execution_count: usize,
    pub fallback_regex_realizations: usize,
    pub profiled_rows: usize,
    pub profile_total_ns: u128,
    pub profile_class_join_ns: u128,
    pub profile_class_regex_ns: u128,
    pub profile_offset_work_ns: u128,
    pub profile_object_join_ns: u128,
    pub profile_direct_execution_ns: u128,
    pub profile_fallback_regex_ns: u128,
}

pub struct Extractor {
    config: ExtractorConfig,
    token_definitions: TokenDefinition,
    token_class_list: TokenClassList,
    token_definition_map: HashMap<String, String>,
    compiled_pattern_cache: Mutex<BoundedCache<String, Arc<CompiledPattern>>>,
    object_plan_cache: Mutex<BoundedCache<ObjectPlanCacheKey, Arc<CachedObjectPlan>>>,
    fallback_regex_cache: Mutex<BoundedCache<String, Arc<Pcre2Regex>>>,
    execution_counters: Mutex<ExecutionCounters>,
}

pub use crate::tel::MatchMode;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ObjectPlanCacheKey {
    pattern_source: String,
    mode: MatchMode,
    captured_groups: Vec<Option<String>>,
    any_prefix_len: Option<usize>,
}

#[derive(Debug, Clone)]
struct CachedObjectPlan {
    steps: Vec<ObjectPlanStep>,
    allow_direct: bool,
    fallback: Arc<ObjectRegexFallback>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ObjectPlanStep {
    Literal {
        tokens: Vec<String>,
    },
    CapturedSpan {
        class_text: Option<String>,
        capture_name: Option<String>,
        is_vanishing: bool,
    },
    SingleToken {
        capture_name: Option<String>,
        is_vanishing: bool,
        consume_trailing_space: bool,
    },
}

#[derive(Debug)]
struct ObjectRegexFallback {
    pattern: Arc<str>,
    variable_names: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct ExecutionCounters {
    direct_execution_attempts: usize,
    direct_execution_hits: usize,
    fallback_execution_count: usize,
    fallback_regex_realizations: usize,
    profiled_rows: usize,
    profile_total_ns: Duration,
    profile_class_join_ns: Duration,
    profile_class_regex_ns: Duration,
    profile_offset_work_ns: Duration,
    profile_object_join_ns: Duration,
    profile_direct_execution_ns: Duration,
    profile_fallback_regex_ns: Duration,
}

#[derive(Debug)]
struct BoundedCache<K: Eq + Hash, V> {
    capacity: usize,
    values: LruCache<K, V>,
    hits: usize,
    misses: usize,
    inserts: usize,
    evictions: usize,
}

impl<K, V> BoundedCache<K, V>
where
    K: Clone + Eq + Hash,
{
    fn new(capacity: usize) -> Self {
        let effective_capacity = capacity.max(1);
        Self {
            capacity,
            values: LruCache::new(
                NonZeroUsize::new(effective_capacity)
                    .expect("effective cache capacity is non-zero"),
            ),
            hits: 0,
            misses: 0,
            inserts: 0,
            evictions: 0,
        }
    }

    fn get_cloned(&mut self, key: &K) -> Option<V>
    where
        V: Clone,
    {
        let Some(value) = self.values.get(key).cloned() else {
            self.misses += 1;
            return None;
        };
        self.hits += 1;
        Some(value)
    }

    fn insert(&mut self, key: K, value: V) {
        if self.capacity == 0 {
            return;
        }

        let existed = self.values.contains(&key);
        let evicted = self.values.push(key, value);
        if evicted.is_some() && !existed {
            self.evictions += 1;
        }
        self.inserts += 1;
    }

    fn stats(&self) -> CacheStats {
        CacheStats {
            capacity: self.capacity,
            len: self.values.len(),
            hits: self.hits,
            misses: self.misses,
            inserts: self.inserts,
            evictions: self.evictions,
        }
    }
}

impl Extractor {
    /// Create a new extractor from token definitions and classes.
    #[must_use]
    pub fn new(token_definitions: TokenDefinition, token_class_list: TokenClassList) -> Self {
        Self::new_with_config(
            token_definitions,
            token_class_list,
            ExtractorConfig::default(),
        )
    }

    /// Create a new extractor with explicit cache configuration.
    #[must_use]
    pub fn new_with_config(
        token_definitions: TokenDefinition,
        token_class_list: TokenClassList,
        config: ExtractorConfig,
    ) -> Self {
        let token_definition_map = token_definitions
            .iter()
            .map(|(name, pattern)| (name.clone(), pattern.clone()))
            .collect();
        Self {
            config,
            token_definitions,
            token_class_list,
            token_definition_map,
            compiled_pattern_cache: Mutex::new(BoundedCache::new(
                config.compiled_pattern_cache_capacity,
            )),
            object_plan_cache: Mutex::new(BoundedCache::new(config.object_plan_cache_capacity)),
            fallback_regex_cache: Mutex::new(BoundedCache::new(
                config.fallback_regex_cache_capacity,
            )),
            execution_counters: Mutex::new(ExecutionCounters::default()),
        }
    }

    #[must_use]
    pub const fn config(&self) -> ExtractorConfig {
        self.config
    }

    /// Return current cache statistics for the extractor instance.
    ///
    /// # Errors
    ///
    /// Returns [`ParseError::InvalidPattern`] if an internal cache mutex is poisoned.
    pub fn stats(&self) -> Result<ExtractorStats, ParseError> {
        let compiled_pattern_cache = self
            .compiled_pattern_cache
            .lock()
            .map_err(|error| {
                ParseError::InvalidPattern(format!("compiled pattern cache poisoned: {error}"))
            })?
            .stats();
        let object_plan_cache = self
            .object_plan_cache
            .lock()
            .map_err(|error| {
                ParseError::InvalidPattern(format!("object plan cache poisoned: {error}"))
            })?
            .stats();
        let (fallback_regex_cache, execution_counters) = {
            let fallback_regex_cache = self
                .fallback_regex_cache
                .lock()
                .map_err(|error| {
                    ParseError::InvalidPattern(format!("fallback regex cache poisoned: {error}"))
                })?
                .stats();
            let execution_counters = *self.execution_counters.lock().map_err(|error| {
                ParseError::InvalidPattern(format!("execution counters poisoned: {error}"))
            })?;
            (fallback_regex_cache, execution_counters)
        };
        let (
            unique_plan_signature_count,
            direct_only_plan_count,
            with_fallback_plan_count,
            total_plan_steps,
            single_token_step_count,
            captured_span_step_count,
            literal_step_count,
        ) = {
            let cache = self.object_plan_cache.lock().map_err(|error| {
                ParseError::InvalidPattern(format!("object plan cache poisoned: {error}"))
            })?;
            let unique_plan_signature_count = cache.values.len();
            let mut direct_only_plan_count = 0;
            let mut with_fallback_plan_count = 0;
            let mut total_plan_steps = 0;
            let mut single_token_step_count = 0;
            let mut captured_span_step_count = 0;
            let mut literal_step_count = 0;
            for (_, plan) in &cache.values {
                if plan.allow_direct {
                    direct_only_plan_count += 1;
                } else {
                    with_fallback_plan_count += 1;
                }
                total_plan_steps += plan.steps.len();
                for step in &plan.steps {
                    match step {
                        ObjectPlanStep::SingleToken { .. } => single_token_step_count += 1,
                        ObjectPlanStep::CapturedSpan { .. } => captured_span_step_count += 1,
                        ObjectPlanStep::Literal { .. } => literal_step_count += 1,
                    }
                }
            }
            (
                unique_plan_signature_count,
                direct_only_plan_count,
                with_fallback_plan_count,
                total_plan_steps,
                single_token_step_count,
                captured_span_step_count,
                literal_step_count,
            )
        };

        Ok(ExtractorStats {
            compiled_pattern_cache,
            object_plan_cache,
            fallback_regex_cache,
            unique_plan_signature_count,
            direct_only_plan_count,
            with_fallback_plan_count,
            total_plan_steps,
            single_token_step_count,
            captured_span_step_count,
            literal_step_count,
            direct_execution_attempts: execution_counters.direct_execution_attempts,
            direct_execution_hits: execution_counters.direct_execution_hits,
            fallback_execution_count: execution_counters.fallback_execution_count,
            fallback_regex_realizations: execution_counters.fallback_regex_realizations,
            profiled_rows: execution_counters.profiled_rows,
            profile_total_ns: execution_counters.profile_total_ns.as_nanos(),
            profile_class_join_ns: execution_counters.profile_class_join_ns.as_nanos(),
            profile_class_regex_ns: execution_counters.profile_class_regex_ns.as_nanos(),
            profile_offset_work_ns: execution_counters.profile_offset_work_ns.as_nanos(),
            profile_object_join_ns: execution_counters.profile_object_join_ns.as_nanos(),
            profile_direct_execution_ns: execution_counters.profile_direct_execution_ns.as_nanos(),
            profile_fallback_regex_ns: execution_counters.profile_fallback_regex_ns.as_nanos(),
        })
    }

    /// Parse the WAN DSL pattern into token metadata.
    ///
    /// # Errors
    ///
    /// Returns [`ParseError::InvalidPattern`] when the pattern contains an unsupported token.
    pub fn compile_pattern(&self, pattern: &str) -> Result<CompiledPattern, ParseError> {
        CompiledPattern::compile(pattern)
    }

    /// Parse the WAN DSL pattern into token metadata.
    ///
    /// # Errors
    ///
    /// Returns [`ParseError::InvalidPattern`] when the pattern contains an unsupported token.
    pub fn extract_token_info(&self, pattern: &str) -> Result<Vec<TokenInfo>, ParseError> {
        Ok(self.compile_pattern(pattern)?.token_info().to_vec())
    }

    /// Parse using pre-tokenized string and class lists.
    ///
    /// # Errors
    ///
    /// Returns [`ParseError::InvalidPattern`] when the generated regex cannot be compiled or the
    /// DSL pattern is invalid.
    #[allow(clippy::too_many_lines)]
    pub fn parse_tokens(
        &self,
        uid: &str,
        obj_string_list: &[String],
        obj_class_list: &[String],
        pattern: &str,
        mode: MatchMode,
    ) -> Result<ParseOutput, ParseError> {
        let compiled_pattern = self.get_or_compile_pattern(pattern)?;
        self.parse_compiled_tokens(
            uid,
            obj_string_list,
            obj_class_list,
            &compiled_pattern,
            mode,
        )
    }

    /// Parse using a precompiled TEL pattern and borrowed class values.
    ///
    /// This avoids forcing callers to materialize owned `String` values for every
    /// class token when they already have a compact or borrowed representation.
    ///
    /// # Errors
    ///
    /// Returns [`ParseError::InvalidPattern`] when the generated regex cannot be compiled.
    pub fn parse_tokens_with_classes<S: AsRef<str>>(
        &self,
        uid: &str,
        obj_string_list: &[String],
        obj_class_list: &[S],
        pattern: &str,
        mode: MatchMode,
    ) -> Result<ParseOutput, ParseError> {
        let compiled_pattern = self.get_or_compile_pattern(pattern)?;
        self.parse_compiled_tokens_with_classes(
            uid,
            obj_string_list,
            obj_class_list,
            &compiled_pattern,
            mode,
        )
    }

    /// Parse using pre-tokenized borrowed token and class lists.
    ///
    /// # Errors
    ///
    /// Returns [`ParseError::InvalidPattern`] when the generated regex cannot be compiled.
    pub fn parse_tokens_with_views<T: AsRef<str>, S: AsRef<str>>(
        &self,
        uid: &str,
        obj_string_list: &[T],
        obj_class_list: &[S],
        pattern: &str,
        mode: MatchMode,
    ) -> Result<ParseOutput, ParseError> {
        let compiled_pattern = self.get_or_compile_pattern(pattern)?;
        self.parse_compiled_tokens_with_views(
            uid,
            obj_string_list,
            obj_class_list,
            &compiled_pattern,
            mode,
        )
    }

    /// Parse using a precompiled TEL pattern.
    ///
    /// # Errors
    ///
    /// Returns [`ParseError::InvalidPattern`] when the generated regex cannot be compiled.
    #[allow(clippy::too_many_lines)]
    pub fn parse_compiled_tokens(
        &self,
        uid: &str,
        obj_string_list: &[String],
        obj_class_list: &[String],
        compiled_pattern: &CompiledPattern,
        mode: MatchMode,
    ) -> Result<ParseOutput, ParseError> {
        self.parse_compiled_tokens_with_views(
            uid,
            obj_string_list,
            obj_class_list,
            compiled_pattern,
            mode,
        )
    }

    /// Parse using a precompiled TEL pattern and borrowed class values.
    ///
    /// # Errors
    ///
    /// Returns [`ParseError::InvalidPattern`] when the generated regex cannot be compiled.
    #[allow(clippy::too_many_lines)]
    pub fn parse_compiled_tokens_with_classes<S: AsRef<str>>(
        &self,
        uid: &str,
        obj_string_list: &[String],
        obj_class_list: &[S],
        compiled_pattern: &CompiledPattern,
        mode: MatchMode,
    ) -> Result<ParseOutput, ParseError> {
        self.parse_compiled_tokens_with_views(
            uid,
            obj_string_list,
            obj_class_list,
            compiled_pattern,
            mode,
        )
    }

    /// Parse using a precompiled TEL pattern and borrowed token/class values.
    ///
    /// # Errors
    ///
    /// Returns [`ParseError::InvalidPattern`] when the generated regex cannot be compiled.
    #[allow(clippy::too_many_lines)]
    pub fn parse_compiled_tokens_with_views<T: AsRef<str>, S: AsRef<str>>(
        &self,
        uid: &str,
        obj_string_list: &[T],
        obj_class_list: &[S],
        compiled_pattern: &CompiledPattern,
        mode: MatchMode,
    ) -> Result<ParseOutput, ParseError> {
        let profiling = profile_enabled();
        let total_start = profiling.then(Instant::now);
        let leading_space_removed = starts_with_space_pair(obj_string_list, obj_class_list);
        let trailing_space_removed = ends_with_space_pair(obj_string_list, obj_class_list);
        let class_join_start = profiling.then(Instant::now);
        let raw_class_string = join_tokens(obj_class_list);
        let obj_class = trim_with_space_flags(
            raw_class_string.as_str(),
            leading_space_removed,
            trailing_space_removed,
        );
        let class_join_elapsed = elapsed_since(class_join_start);

        let class_pattern = compiled_pattern.class_pattern(mode);
        let class_regex = compiled_pattern.class_regex(mode)?;
        let class_regex_start = profiling.then(Instant::now);
        let class_captures =
            run_pcre2_captures(class_regex, obj_class, "class comparator", class_pattern)?;
        let class_regex_elapsed = elapsed_since(class_regex_start);

        let Some(class_match) = class_captures else {
            self.record_profile_timing(ProfileTiming {
                rows: 1,
                total: elapsed_since(total_start),
                class_join: class_join_elapsed,
                class_regex: class_regex_elapsed,
                ..ProfileTiming::default()
            })?;
            return Ok(ParseOutput {
                uid: uid.to_string(),
                fields: HashMap::new(),
                complement: join_tokens(obj_string_list),
            });
        };

        let raw_groups = capture_groups(&class_match);
        if raw_groups.iter().any(|group| group.as_deref() == Some(" ")) {
            self.record_profile_timing(ProfileTiming {
                rows: 1,
                total: elapsed_since(total_start),
                class_join: class_join_elapsed,
                class_regex: class_regex_elapsed,
                ..ProfileTiming::default()
            })?;
            return Ok(ParseOutput {
                uid: uid.to_string(),
                fields: HashMap::new(),
                complement: join_tokens(obj_string_list),
            });
        }

        let offset_work_start = profiling.then(Instant::now);
        let class_offsets = token_offsets_ref(obj_class_list);
        let object_offsets = token_offsets_ref(obj_string_list);
        let left_trim = if leading_space_removed {
            obj_class_list
                .first()
                .map_or(0, |token| token.as_ref().len())
        } else {
            0
        };

        let (aligned_obj_start, _aligned_obj_end) = if mode == MatchMode::Any {
            align_any_match(
                &class_match,
                &class_offsets,
                &object_offsets,
                obj_class_list,
                left_trim,
            )
        } else {
            (None, None)
        };

        let captured_multi_or_optional_groups =
            filter_class_groups(&raw_groups, compiled_pattern.class_segments());

        let object_plan = self.get_or_build_object_plan(
            compiled_pattern,
            captured_multi_or_optional_groups.as_deref().unwrap_or(&[]),
            mode,
            if mode == MatchMode::Any {
                aligned_obj_start
            } else {
                None
            },
        )?;

        let full_match = class_match.get(0).ok_or_else(|| {
            ParseError::InvalidPattern(
                "class comparator matched without a full match group".to_string(),
            )
        })?;
        let Some(match_range) = match_token_index_range(
            &class_offsets,
            left_trim,
            full_match.start(),
            full_match.end(),
        ) else {
            self.record_profile_timing(ProfileTiming {
                rows: 1,
                total: elapsed_since(total_start),
                class_join: class_join_elapsed,
                class_regex: class_regex_elapsed,
                offset_work: elapsed_since(offset_work_start),
                ..ProfileTiming::default()
            })?;
            return Ok(ParseOutput {
                uid: uid.to_string(),
                fields: HashMap::new(),
                complement: join_tokens(obj_string_list),
            });
        };
        let offset_work_elapsed = elapsed_since(offset_work_start);

        let object_join_start = profiling.then(Instant::now);
        let full_obj_string = join_tokens(obj_string_list);
        let obj_string = trim_with_space_flags(
            full_obj_string.as_str(),
            leading_space_removed,
            trailing_space_removed,
        );
        let object_join_elapsed = elapsed_since(object_join_start);

        let direct_execution_start = profiling.then(Instant::now);
        let direct_execution = if leading_space_removed {
            None
        } else {
            self.record_direct_execution_attempt()?;
            execute_object_plan(
                &object_plan,
                obj_string_list,
                obj_class_list,
                obj_string,
                &object_offsets,
                match_range,
            )
        };
        let direct_execution_elapsed = elapsed_since(direct_execution_start);

        let mut fallback_regex_elapsed = Duration::ZERO;
        let (fields, mut complement) = if let Some(execution) = direct_execution {
            self.record_direct_execution_hit()?;
            (
                execution.fields,
                get_complement_of_spans(obj_string, &execution.capture_spans)
                    .trim_start()
                    .to_string(),
            )
        } else {
            self.record_fallback_execution()?;
            let fallback_regex =
                self.get_or_compile_fallback_regex(object_plan.fallback.pattern.as_ref())?;
            let fallback_regex_start = profiling.then(Instant::now);
            let obj_captures = run_pcre2_captures(
                fallback_regex.as_ref(),
                obj_string,
                "object comparator",
                object_plan.fallback.pattern.as_ref(),
            )?;
            fallback_regex_elapsed = elapsed_since(fallback_regex_start);

            let Some(obj_match) = obj_captures else {
                self.record_profile_timing(ProfileTiming {
                    rows: 1,
                    total: elapsed_since(total_start),
                    class_join: class_join_elapsed,
                    class_regex: class_regex_elapsed,
                    offset_work: offset_work_elapsed,
                    object_join: object_join_elapsed,
                    direct_execution: direct_execution_elapsed,
                    fallback_regex: fallback_regex_elapsed,
                })?;
                return Ok(ParseOutput {
                    uid: uid.to_string(),
                    fields: HashMap::new(),
                    complement: full_obj_string,
                });
            };

            let mut fields = HashMap::new();
            for (index, name) in object_plan
                .fallback
                .variable_names
                .as_slice()
                .iter()
                .enumerate()
            {
                if let Some(value) = obj_match
                    .get(index + 1)
                    .and_then(|matched| std::str::from_utf8(matched.as_bytes()).ok())
                    .map(str::trim)
                {
                    if value.is_empty() {
                        continue;
                    }
                    fields
                        .entry(name.clone())
                        .and_modify(|existing: &mut String| {
                            existing.push(' ');
                            existing.push_str(value);
                        })
                        .or_insert_with(|| value.to_string());
                }
            }

            (
                fields,
                get_complement_of_captured_groups(obj_string, &obj_match),
            )
        };

        if leading_space_removed {
            complement.insert(0, ' ');
        }

        self.record_profile_timing(ProfileTiming {
            rows: 1,
            total: elapsed_since(total_start),
            class_join: class_join_elapsed,
            class_regex: class_regex_elapsed,
            offset_work: offset_work_elapsed,
            object_join: object_join_elapsed,
            direct_execution: direct_execution_elapsed,
            fallback_regex: fallback_regex_elapsed,
        })?;

        Ok(ParseOutput {
            uid: uid.to_string(),
            fields,
            complement,
        })
    }

    /// Compatibility wrapper that tokenizes the input and parses using whole-string matching.
    ///
    /// # Errors
    ///
    /// Returns [`ParseError::InvalidPattern`] when the pattern cannot be compiled.
    pub fn parse_string(
        &self,
        raw_value: &str,
        pattern: &str,
    ) -> Result<(String, HashMap<String, String>, String), ParseError> {
        let tokenized = tokenize_and_classify(
            raw_value,
            &self.token_definitions,
            Some(&self.token_class_list),
        );
        let output = self.parse_tokens(
            raw_value,
            &tokenized.tokens,
            &tokenized.classes,
            pattern,
            MatchMode::Whole,
        )?;
        Ok((output.uid, output.fields, output.complement))
    }
}

#[derive(Debug, Clone)]
struct DirectExecutionResult {
    fields: HashMap<String, String>,
    capture_spans: Vec<(usize, usize)>,
}

fn create_object_plan_steps(
    augmented_extracted_token_info: &[CompiledClassSegment],
    captured_multi_groups_optional: Option<Vec<Option<String>>>,
) -> (Vec<ObjectPlanStep>, bool) {
    let mut captured_multi_groups_optional = captured_multi_groups_optional.unwrap_or_default();
    let mut steps = Vec::with_capacity(augmented_extracted_token_info.len());
    let mut requires_regex_fallback = false;

    for token_info in augmented_extracted_token_info {
        let segment = &token_info.segment;
        let token = &segment.token_info.token;

        if segment.token_info.kind == crate::tel::TokenKind::Literal {
            let literal_tokens = split_input_tokens(token);
            requires_regex_fallback = true;
            steps.push(ObjectPlanStep::Literal {
                tokens: literal_tokens,
            });
            continue;
        }

        let flags = segment.token_info.flags;
        let needs_captured_class = flags.multi_group || flags.optional || !flags.strict_class;
        if needs_captured_class {
            let captured = if captured_multi_groups_optional.is_empty() {
                None
            } else {
                captured_multi_groups_optional.remove(0)
            };
            let can_collapse_to_single = !flags.multi_group
                && captured
                    .as_deref()
                    .is_some_and(|value| !value.contains(char::is_whitespace));
            if can_collapse_to_single {
                steps.push(ObjectPlanStep::SingleToken {
                    capture_name: segment.token_info.var_name.clone(),
                    is_vanishing: segment.token_info.is_vanishing_group(),
                    consume_trailing_space: true,
                });
                continue;
            }
            requires_regex_fallback = true;
            steps.push(ObjectPlanStep::CapturedSpan {
                class_text: captured,
                capture_name: segment.token_info.var_name.clone(),
                is_vanishing: segment.token_info.is_vanishing_group(),
            });
            continue;
        }

        steps.push(ObjectPlanStep::SingleToken {
            capture_name: segment.token_info.var_name.clone(),
            is_vanishing: segment.token_info.is_vanishing_group(),
            consume_trailing_space: true,
        });
    }

    (steps, requires_regex_fallback)
}

fn execute_object_plan(
    plan: &CachedObjectPlan,
    obj_string_list: &[impl AsRef<str>],
    obj_class_list: &[impl AsRef<str>],
    obj_string: &str,
    object_offsets: &[(usize, usize)],
    match_range: (usize, usize),
) -> Option<DirectExecutionResult> {
    let execution = execute_object_plan_steps(
        &plan.steps,
        obj_string_list,
        obj_class_list,
        obj_string,
        object_offsets,
        match_range,
    )?;

    if !plan.allow_direct {
        return None;
    }

    // Validate the reconstructed spans against the current object string bounds.
    if execution
        .capture_spans
        .iter()
        .any(|(start, end)| start > end || *end > obj_string.len())
    {
        return None;
    }

    Some(execution)
}

fn execute_object_plan_steps(
    steps: &[ObjectPlanStep],
    obj_string_list: &[impl AsRef<str>],
    obj_class_list: &[impl AsRef<str>],
    obj_string: &str,
    object_offsets: &[(usize, usize)],
    match_range: (usize, usize),
) -> Option<DirectExecutionResult> {
    let mut current = match_range.0;
    let mut fields = HashMap::new();
    let mut capture_spans = Vec::new();

    for step in steps {
        match step {
            ObjectPlanStep::Literal { tokens } => {
                let start = skip_whitespace_tokens(obj_string_list, current, match_range.1);
                let end = start + tokens.len();
                if end > match_range.1 {
                    return None;
                }
                if !obj_string_list[start..end]
                    .iter()
                    .map(AsRef::as_ref)
                    .eq(tokens.iter().map(String::as_str))
                {
                    return None;
                }
                current = end;
            }
            ObjectPlanStep::CapturedSpan {
                class_text,
                capture_name,
                is_vanishing,
            } => {
                let Some(class_text) = class_text.as_deref() else {
                    continue;
                };
                if class_text.is_empty() {
                    continue;
                }

                let start = skip_whitespace_tokens(obj_class_list, current, match_range.1);
                let end = consume_class_text(obj_class_list, start, match_range.1, class_text)?;
                if let Some(name) = capture_name
                    && !is_vanishing
                {
                    let span = object_span_for_tokens(object_offsets, start, end);
                    if let Some((span_start, span_end)) = span {
                        append_field_from_span(&mut fields, name, span_start, span_end, obj_string);
                        capture_spans.push((span_start, span_end));
                    }
                }
                current = end;
            }
            ObjectPlanStep::SingleToken {
                capture_name,
                is_vanishing,
                consume_trailing_space,
            } => {
                let start = skip_whitespace_tokens(obj_class_list, current, match_range.1);
                if start >= match_range.1 {
                    return None;
                }
                let mut end = start + 1;
                if *consume_trailing_space {
                    while end < match_range.1
                        && obj_class_list[end]
                            .as_ref()
                            .chars()
                            .all(char::is_whitespace)
                    {
                        end += 1;
                    }
                }
                if let Some(name) = capture_name
                    && !is_vanishing
                {
                    let span = object_span_for_tokens(object_offsets, start, end);
                    if let Some((span_start, span_end)) = span {
                        append_field_from_span(&mut fields, name, span_start, span_end, obj_string);
                        capture_spans.push((span_start, span_end));
                    }
                }
                current = end;
            }
        }
    }

    Some(DirectExecutionResult {
        fields,
        capture_spans,
    })
}

fn skip_whitespace_tokens<S: AsRef<str>>(tokens: &[S], mut index: usize, end: usize) -> usize {
    while index < end && tokens[index].as_ref().chars().all(char::is_whitespace) {
        index += 1;
    }
    index
}

fn consume_class_text<S: AsRef<str>>(
    obj_class_list: &[S],
    start: usize,
    end: usize,
    class_text: &str,
) -> Option<usize> {
    let mut accumulated = String::new();
    for (index, token) in obj_class_list.iter().enumerate().take(end).skip(start) {
        accumulated.push_str(token.as_ref());
        if accumulated == class_text {
            return Some(index + 1);
        }
        if !class_text.starts_with(&accumulated) {
            return None;
        }
    }
    None
}

fn object_span_for_tokens(
    object_offsets: &[(usize, usize)],
    start: usize,
    end: usize,
) -> Option<(usize, usize)> {
    if start >= end {
        return None;
    }
    Some((object_offsets.get(start)?.0, object_offsets.get(end - 1)?.1))
}

fn append_field_from_span(
    fields: &mut HashMap<String, String>,
    name: &str,
    start: usize,
    end: usize,
    obj_string: &str,
) {
    let Some(value) = obj_string.get(start..end).map(str::trim) else {
        return;
    };
    let value = value.to_string();
    if value.is_empty() {
        return;
    }
    fields
        .entry(name.to_string())
        .and_modify(|existing| {
            existing.push(' ');
            existing.push_str(&value);
        })
        .or_insert(value);
}

fn match_token_index_range(
    class_offsets: &[(usize, usize)],
    left_trim: usize,
    match_start: usize,
    match_end: usize,
) -> Option<(usize, usize)> {
    let raw_start = left_trim + match_start;
    let raw_end = left_trim + match_end;

    let mut start_index = None;
    let mut end_index = None;
    for (index, (token_start, token_end)) in class_offsets.iter().enumerate() {
        if start_index.is_none() && *token_start <= raw_start && raw_start < *token_end {
            start_index = Some(index);
        }
        if raw_end <= *token_end && *token_start < raw_end {
            end_index = Some(index + 1);
            break;
        }
    }

    match (start_index, end_index) {
        (Some(start), Some(end)) if start < end => Some((start, end)),
        _ => None,
    }
}

impl Extractor {
    fn get_or_compile_pattern(&self, pattern: &str) -> Result<Arc<CompiledPattern>, ParseError> {
        let cached = self
            .compiled_pattern_cache
            .lock()
            .map_err(|error| {
                ParseError::InvalidPattern(format!("compiled pattern cache poisoned: {error}"))
            })?
            .get_cloned(&pattern.to_string());
        if let Some(compiled) = cached {
            return Ok(compiled);
        }

        let compiled = Arc::new(self.compile_pattern(pattern)?);
        self.compiled_pattern_cache
            .lock()
            .map_err(|error| {
                ParseError::InvalidPattern(format!("compiled pattern cache poisoned: {error}"))
            })?
            .insert(pattern.to_string(), Arc::clone(&compiled));
        Ok(compiled)
    }

    fn get_or_build_object_plan(
        &self,
        compiled_pattern: &CompiledPattern,
        captured_groups: &[Option<String>],
        mode: MatchMode,
        any_prefix_len: Option<usize>,
    ) -> Result<Arc<CachedObjectPlan>, ParseError> {
        let key = ObjectPlanCacheKey {
            pattern_source: compiled_pattern.source().to_string(),
            mode,
            captured_groups: captured_groups.to_vec(),
            any_prefix_len,
        };

        let cached = self
            .object_plan_cache
            .lock()
            .map_err(|error| {
                ParseError::InvalidPattern(format!("object plan cache poisoned: {error}"))
            })?
            .get_cloned(&key);
        if let Some(plan) = cached {
            return Ok(plan);
        }

        let (steps, requires_regex_fallback) = create_object_plan_steps(
            compiled_pattern.class_segments(),
            if captured_groups.is_empty() {
                None
            } else {
                Some(captured_groups.to_vec())
            },
        );

        let (mut comparator, augmented_info) = create_obj_comparator_string(
            compiled_pattern.class_segments(),
            if captured_groups.is_empty() {
                None
            } else {
                Some(captured_groups.to_vec())
            },
            &self.token_definition_map,
        );
        comparator = apply_match_mode(&comparator, mode);
        if mode == MatchMode::Any
            && let Some(start) = any_prefix_len
        {
            comparator = if start > 0 {
                format!(r"(?s)^(?:.{{{start}}}){comparator}(?:.*)$")
            } else {
                format!(r"(?s)^{comparator}(?:.*)$")
            };
        }
        let variable_names = augmented_info
            .iter()
            .filter(|info| {
                !info.segment.token_info.is_vanishing_group()
                    && (info.segment.token_info.is_capturing_group()
                        || info.segment.token_info.flags.optional)
                    && info
                        .regex_pattern
                        .as_deref()
                        .is_some_and(|pattern| !pattern.is_empty())
            })
            .filter_map(|info| info.segment.token_info.var_name.clone())
            .collect();
        let fallback = Arc::new(ObjectRegexFallback {
            pattern: Arc::<str>::from(comparator),
            variable_names,
        });

        let plan = Arc::new(CachedObjectPlan {
            steps,
            allow_direct: !requires_regex_fallback,
            fallback,
        });

        self.object_plan_cache
            .lock()
            .map_err(|error| {
                ParseError::InvalidPattern(format!("object plan cache poisoned: {error}"))
            })?
            .insert(key, Arc::clone(&plan));
        Ok(plan)
    }

    fn get_or_compile_fallback_regex(&self, pattern: &str) -> Result<Arc<Pcre2Regex>, ParseError> {
        if let Some(regex) = self
            .fallback_regex_cache
            .lock()
            .map_err(|error| {
                ParseError::InvalidPattern(format!("fallback regex cache poisoned: {error}"))
            })?
            .get_cloned(&pattern.to_string())
        {
            return Ok(regex);
        }

        let compiled = Arc::new(compile_pcre2_regex(pattern, "object comparator")?);
        self.fallback_regex_cache
            .lock()
            .map_err(|error| {
                ParseError::InvalidPattern(format!("fallback regex cache poisoned: {error}"))
            })?
            .insert(pattern.to_string(), Arc::clone(&compiled));
        self.execution_counters
            .lock()
            .map_err(|error| {
                ParseError::InvalidPattern(format!("execution counters poisoned: {error}"))
            })?
            .fallback_regex_realizations += 1;
        Ok(compiled)
    }

    fn record_direct_execution_attempt(&self) -> Result<(), ParseError> {
        self.execution_counters
            .lock()
            .map_err(|error| {
                ParseError::InvalidPattern(format!("execution counters poisoned: {error}"))
            })?
            .direct_execution_attempts += 1;
        Ok(())
    }

    fn record_direct_execution_hit(&self) -> Result<(), ParseError> {
        self.execution_counters
            .lock()
            .map_err(|error| {
                ParseError::InvalidPattern(format!("execution counters poisoned: {error}"))
            })?
            .direct_execution_hits += 1;
        Ok(())
    }

    fn record_fallback_execution(&self) -> Result<(), ParseError> {
        self.execution_counters
            .lock()
            .map_err(|error| {
                ParseError::InvalidPattern(format!("execution counters poisoned: {error}"))
            })?
            .fallback_execution_count += 1;
        Ok(())
    }

    fn record_profile_timing(&self, timing: ProfileTiming) -> Result<(), ParseError> {
        if !profile_enabled() {
            return Ok(());
        }
        let mut counters = self.execution_counters.lock().map_err(|error| {
            ParseError::InvalidPattern(format!("execution counters poisoned: {error}"))
        })?;
        counters.profiled_rows += timing.rows;
        counters.profile_total_ns += timing.total;
        counters.profile_class_join_ns += timing.class_join;
        counters.profile_class_regex_ns += timing.class_regex;
        counters.profile_offset_work_ns += timing.offset_work;
        counters.profile_object_join_ns += timing.object_join;
        counters.profile_direct_execution_ns += timing.direct_execution;
        counters.profile_fallback_regex_ns += timing.fallback_regex;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct ProfileTiming {
    rows: usize,
    total: Duration,
    class_join: Duration,
    class_regex: Duration,
    offset_work: Duration,
    object_join: Duration,
    direct_execution: Duration,
    fallback_regex: Duration,
}

fn starts_with_space_pair(
    obj_string_list: &[impl AsRef<str>],
    obj_class_list: &[impl AsRef<str>],
) -> bool {
    !obj_string_list.is_empty()
        && !obj_class_list.is_empty()
        && obj_string_list[0].as_ref().chars().all(char::is_whitespace)
        && obj_class_list[0].as_ref().chars().all(char::is_whitespace)
}

fn ends_with_space_pair(
    obj_string_list: &[impl AsRef<str>],
    obj_class_list: &[impl AsRef<str>],
) -> bool {
    !obj_string_list.is_empty()
        && !obj_class_list.is_empty()
        && obj_string_list[obj_string_list.len() - 1]
            .as_ref()
            .chars()
            .all(char::is_whitespace)
        && obj_class_list[obj_class_list.len() - 1]
            .as_ref()
            .chars()
            .all(char::is_whitespace)
}

fn capture_groups(captures: &Pcre2Captures) -> Vec<Option<String>> {
    (1..captures.len())
        .map(|index| {
            captures.get(index).and_then(|matched| {
                std::str::from_utf8(matched.as_bytes())
                    .ok()
                    .map(ToString::to_string)
            })
        })
        .collect()
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

fn run_pcre2_captures<'a>(
    regex: &Pcre2Regex,
    text: &'a str,
    label: &str,
    pattern: &str,
) -> Result<Option<Pcre2Captures<'a>>, ParseError> {
    regex.captures(text.as_bytes()).map_err(|error| {
        ParseError::InvalidPattern(format!("error running {label} '{pattern}': {error}"))
    })
}

fn filter_class_groups(
    raw_groups: &[Option<String>],
    augmented_extracted_token_info: &[CompiledClassSegment],
) -> Option<Vec<Option<String>>> {
    if raw_groups.is_empty() {
        return None;
    }

    let mut filtered_groups: Vec<Option<String>> = Vec::new();
    let mut group_index = 0_usize;
    let total_groups = raw_groups.len();

    for token_info in augmented_extracted_token_info {
        let group_count = token_info.capturing_group_count;
        if group_count == 0 {
            continue;
        }

        let next_index = group_index + group_count;
        if next_index > total_groups {
            break;
        }

        let group_slice = &raw_groups[group_index..next_index];
        let flags = token_info.segment.token_info.flags;
        if flags.multi_group
            || flags.optional
            || !flags.strict_class
            || token_info.segment.token_info.is_vanishing_group()
        {
            filtered_groups.extend(group_slice.iter().cloned());
        }

        group_index = next_index;
    }

    filtered_groups.extend(raw_groups[group_index..].iter().cloned());

    if filtered_groups.is_empty() {
        None
    } else {
        Some(filtered_groups)
    }
}

fn align_any_match(
    class_match: &Pcre2Captures,
    class_offsets: &[(usize, usize)],
    obj_offsets: &[(usize, usize)],
    obj_class_list: &[impl AsRef<str>],
    left_trim: usize,
) -> (Option<usize>, Option<usize>) {
    let Some(full_match) = class_match.get(0) else {
        return (None, None);
    };

    let class_start_raw = left_trim + full_match.start();
    let class_end_raw = left_trim + full_match.end();
    let mut start_token_index = None;
    let mut end_token_index = None;

    for (index, (start, end)) in class_offsets.iter().enumerate() {
        if start_token_index.is_none() && *start <= class_start_raw && class_start_raw < *end {
            start_token_index = Some(index);
        }
        if *start < class_end_raw && class_end_raw <= *end {
            end_token_index = Some(index);
        }
    }

    let (Some(mut start_token_index), Some(mut end_token_index)) =
        (start_token_index, end_token_index)
    else {
        return (None, None);
    };

    while start_token_index < obj_class_list.len()
        && obj_class_list[start_token_index].as_ref().trim().is_empty()
    {
        start_token_index += 1;
    }
    while end_token_index > 0 && obj_class_list[end_token_index].as_ref().trim().is_empty() {
        end_token_index -= 1;
    }

    if start_token_index > end_token_index {
        return (None, None);
    }

    (
        obj_offsets.get(start_token_index).map(|(start, _)| *start),
        obj_offsets.get(end_token_index).map(|(_, end)| *end),
    )
}

fn trim_with_space_flags(
    text: &str,
    leading_space_removed: bool,
    trailing_space_removed: bool,
) -> &str {
    let text = if leading_space_removed {
        text.trim_start()
    } else {
        text
    };
    if trailing_space_removed {
        text.trim_end()
    } else {
        text
    }
}

fn token_offsets_ref<S: AsRef<str>>(tokens: &[S]) -> Vec<(usize, usize)> {
    let mut offset = 0_usize;
    let mut result = Vec::with_capacity(tokens.len());
    for token in tokens {
        let start = offset;
        offset += token.as_ref().len();
        result.push((start, offset));
    }
    result
}

fn join_tokens<T: AsRef<str>>(tokens: &[T]) -> String {
    let total_len = tokens.iter().map(|token| token.as_ref().len()).sum();
    let mut out = String::with_capacity(total_len);
    for token in tokens {
        out.push_str(token.as_ref());
    }
    out
}

fn profile_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("TOKMAT_PROFILE")
            .map(|value| value != "0" && !value.is_empty())
            .unwrap_or(false)
    })
}

fn elapsed_since(start: Option<Instant>) -> Duration {
    start.map_or(Duration::ZERO, |start| start.elapsed())
}

fn strip_word_boundaries(pattern: &str) -> &str {
    pattern
        .strip_prefix(WORD_BOUNDARY_REGEX)
        .and_then(|stripped| stripped.strip_suffix(WORD_BOUNDARY_REGEX))
        .unwrap_or(pattern)
}

fn create_obj_comparator_string(
    augmented_extracted_token_info: &[CompiledClassSegment],
    captured_multi_groups_optional: Option<Vec<Option<String>>>,
    token_definitions: &HashMap<String, String>,
) -> (String, Vec<ObjComparatorTokenInfo>) {
    let mut captured_multi_groups_optional = captured_multi_groups_optional.unwrap_or_default();
    let mut comparator_parts = Vec::new();
    let mut augmented = Vec::new();

    for token_info in augmented_extracted_token_info {
        let token = &token_info.segment.token_info.token;
        if token_info.segment.token_info.kind == crate::tel::TokenKind::Literal {
            let escaped = escape_regex_literal(token);
            comparator_parts.push(escaped.clone());
            augmented.push(ObjComparatorTokenInfo {
                segment: token_info.segment.clone(),
                class_comparator_substring: token_info.class_comparator_substring.clone(),
                multi_group_optional: None,
                new_class_type: None,
                regex_pattern: Some(escaped),
            });
            continue;
        }

        let flags = token_info.segment.token_info.flags;
        let needs_captured_class = flags.multi_group || flags.optional || !flags.strict_class;
        let (multi_group_optional, new_class_type) = if needs_captured_class {
            if captured_multi_groups_optional.is_empty() {
                continue;
            }
            let captured = captured_multi_groups_optional.remove(0);
            let new_class_type = captured.as_ref().and_then(|value| {
                if value.is_empty() {
                    None
                } else {
                    Some(value.clone())
                }
            });
            (captured, new_class_type)
        } else {
            (None, token_info.segment.token_info.class_type.clone())
        };

        let regex_pattern = create_regex_pattern(
            token,
            new_class_type.as_deref(),
            &token_info.segment,
            token_definitions,
        );
        comparator_parts.push(regex_pattern.clone());
        augmented.push(ObjComparatorTokenInfo {
            segment: token_info.segment.clone(),
            class_comparator_substring: token_info.class_comparator_substring.clone(),
            multi_group_optional,
            new_class_type,
            regex_pattern: Some(regex_pattern),
        });
    }

    (comparator_parts.join(r"\s*"), augmented)
}

#[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
fn create_regex_pattern(
    token: &str,
    class_type_string: Option<&str>,
    segment: &TelSegment,
    token_definitions: &HashMap<String, String>,
) -> String {
    let token_info = &segment.token_info;
    let is_capturing_group = token_info.is_capturing_group();
    let is_optional = matches!(segment.quantity, Quantity::Optional);
    let is_vanishing_group = token_info.is_vanishing_group();

    if is_optional && class_type_string.is_none() {
        return String::new();
    }
    if class_type_string == Some("") {
        return escape_regex_literal(token);
    }

    let resolved_class_type = if is_vanishing_group && class_type_string.is_none() {
        token_info.class_type.clone()
    } else {
        class_type_string.map(ToOwned::to_owned)
    };

    let Some(class_type_string) = resolved_class_type else {
        return String::new();
    };

    let mut regex_fragments = Vec::new();
    for class_type in class_type_string.split_whitespace() {
        let fragment =
            resolve_class_pattern(class_type, segment, token_definitions).unwrap_or_default();
        regex_fragments.push(fragment);
    }

    let mut final_regex = format!(r"{}\s*", regex_fragments.join(r"\s*"));
    final_regex = wrap_regex_group(
        &final_regex,
        is_capturing_group,
        is_optional,
        is_vanishing_group,
    );

    if is_capturing_group || is_vanishing_group {
        final_regex
    } else {
        replace_literal_token_prefix(token, final_regex.as_str())
    }
}

fn resolve_class_pattern(
    class_type: &str,
    segment: &TelSegment,
    token_definitions: &HashMap<String, String>,
) -> Option<String> {
    expand_dictionary_class_type(class_type, segment, token_definitions)
        .or_else(|| modifier_only_fallback(segment))
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
        if character.is_alphanumeric()
            || character == '_'
            || matches!(character, '@' | '#' | ',' | '+' | '?' | '|')
        {
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

fn expand_dictionary_class_type(
    class_type: &str,
    segment: &TelSegment,
    token_definitions: &HashMap<String, String>,
) -> Option<String> {
    if class_type.is_empty() || token_definitions.is_empty() {
        return None;
    }

    let class_type_list: Vec<&str> = if class_type.starts_with('(') && class_type.ends_with(')') {
        class_type
            .get(3..class_type.len().saturating_sub(1))
            .unwrap_or_default()
            .split('|')
            .collect()
    } else {
        vec![strip_word_boundaries(class_type)]
    };

    let mut regex_patterns = Vec::new();
    let mut fallback_pattern = None;
    for class_name in class_type_list {
        if let Some(pattern) = token_definitions.get(class_name) {
            regex_patterns.push(trim_regex_anchors(pattern).to_string());
        } else {
            fallback_pattern = convert_segment_type_modifier_to_regex(segment);
        }
    }

    if regex_patterns.is_empty() {
        fallback_pattern
    } else {
        Some(format!("(?:{})", regex_patterns.join("|")))
    }
}

fn modifier_only_fallback(segment: &TelSegment) -> Option<String> {
    let modifier = segment.token_info.modifier.as_deref();
    if modifier.is_some_and(|value| {
        value
            .chars()
            .any(|character| matches!(character, '%' | '=' | '$' | '['))
    }) {
        let temp = apply_segment_to_class_type(segment, None);
        if temp.is_some() {
            return temp;
        }
    }
    convert_segment_type_modifier_to_regex(segment)
}

fn trim_regex_anchors(pattern: &str) -> &str {
    let without_start = pattern.strip_prefix('^').unwrap_or(pattern);
    without_start.strip_suffix('$').unwrap_or(without_start)
}

fn wrap_regex_group(
    base_regex: &str,
    is_capturing_group: bool,
    is_optional: bool,
    is_vanishing_group: bool,
) -> String {
    if is_vanishing_group {
        let base_regex = make_non_capturing(base_regex);
        if is_optional {
            format!("({base_regex})?")
        } else {
            base_regex
        }
    } else if is_capturing_group {
        let base_regex = format!("({base_regex})");
        if is_optional {
            format!("{base_regex}?")
        } else {
            base_regex
        }
    } else {
        let base_regex = make_non_capturing(base_regex);
        if is_optional {
            format!("({base_regex})?")
        } else {
            base_regex
        }
    }
}

fn make_non_capturing(pattern: &str) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    let mut output = String::with_capacity(pattern.len());
    let mut index = 0_usize;
    let mut escaped = false;

    while index < chars.len() {
        let character = chars[index];
        if escaped {
            output.push(character);
            escaped = false;
            index += 1;
            continue;
        }
        if character == '\\' {
            output.push(character);
            escaped = true;
            index += 1;
            continue;
        }
        if character == '(' {
            if index + 1 < chars.len() && chars[index + 1] == '?' {
                if index + 3 < chars.len()
                    && chars[index + 1] == '?'
                    && chars[index + 2] == 'P'
                    && chars[index + 3] == '<'
                {
                    output.push_str("(?:");
                    index += 4;
                    while index < chars.len() && chars[index] != '>' {
                        index += 1;
                    }
                    if index < chars.len() && chars[index] == '>' {
                        index += 1;
                    }
                    continue;
                }
                output.push(character);
                index += 1;
                continue;
            }
            output.push_str("(?:");
            index += 1;
            continue;
        }

        output.push(character);
        index += 1;
    }

    output
}

fn get_complement_of_captured_groups(text: &str, matched: &Pcre2Captures) -> String {
    let mut complement_parts = Vec::new();
    let mut start = 0_usize;

    let mut spans = Vec::new();
    for index in 1..matched.len() {
        if let Some(group) = matched.get(index) {
            spans.push((group.start(), group.end()));
        }
    }
    spans.sort_unstable();

    for (group_start, group_end) in spans {
        if group_start > start {
            complement_parts.push(&text[start..group_start]);
        }
        start = group_end;
    }

    if start < text.len() {
        complement_parts.push(&text[start..]);
    }

    complement_parts.concat()
}

fn get_complement_of_spans(text: &str, spans: &[(usize, usize)]) -> String {
    if spans.is_empty() {
        return text.to_string();
    }

    let mut sorted_spans = spans.to_vec();
    sorted_spans.sort_unstable();

    let mut complement_parts = Vec::new();
    let mut start = 0_usize;
    for (span_start, span_end) in sorted_spans {
        if span_start > start {
            complement_parts.push(&text[start..span_start]);
        }
        start = start.max(span_end);
    }
    if start < text.len() {
        complement_parts.push(&text[start..]);
    }

    complement_parts.concat()
}

fn convert_segment_type_modifier_to_regex(segment: &TelSegment) -> Option<String> {
    match filter_for_class_type_modifier(segment).as_deref() {
        None => Some(DEFAULT_WORD_REGEX.to_string()),
        Some("@") => Some(r"[a-zA-Z]+".to_string()),
        Some("#") => Some(r"[\d]+".to_string()),
        Some(",") => Some(r"[,\-:;]+".to_string()),
        _ => None,
    }
}

fn filter_for_class_type_modifier(segment: &TelSegment) -> Option<String> {
    let mut filtered = String::new();
    if segment.type_modifiers.alpha {
        filtered.push('@');
    }
    if segment.type_modifiers.numeric {
        filtered.push('#');
    }
    if segment
        .token_info
        .modifier
        .as_deref()
        .is_some_and(|value| value.contains(','))
    {
        filtered.push(',');
    }
    if filtered.is_empty() {
        None
    } else {
        Some(filtered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tel::{TokenKind, split_parse_tokens};
    use std::collections::HashSet;

    fn mock_extractor() -> Extractor {
        let defs = vec![
            ("NUM".to_string(), r"^\d+$".to_string()),
            ("ALPHA".to_string(), r"^[A-Z]+$".to_string()),
            ("STREETTYPE".to_string(), r"^(?:ST|AVE)$".to_string()),
            ("PROV".to_string(), r"^(?:NS|ON)$".to_string()),
        ];
        let classes = vec![(
            "STREETTYPE".to_string(),
            vec!["ST", "AVE"]
                .into_iter()
                .map(String::from)
                .collect::<HashSet<_>>(),
        )];
        Extractor::new(defs, classes)
    }

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
    fn test_extract_token_info_handles_capturing_and_literals() {
        let extractor = mock_extractor();
        let infos = extractor
            .extract_token_info("<<CIVIC#>> \"<<TITLE>>\" <<LAST>>")
            .expect("pattern should parse");
        assert_eq!(infos.len(), 5);
        assert_eq!(infos[0].var_name.as_deref(), Some("CIVIC"));
        assert!(infos[0].is_capturing_group());
        assert_eq!(infos[1].kind, TokenKind::Literal);
        assert_eq!(infos[2].var_name.as_deref(), Some("TITLE"));
    }

    #[test]
    fn test_parse_tokens_matches_python_like_simple_address() {
        let extractor = mock_extractor();
        let tokens = vec![
            "123".to_string(),
            " ".to_string(),
            "MAIN".to_string(),
            " ".to_string(),
            "ST".to_string(),
        ];
        let classes = vec![
            "NUM".to_string(),
            " ".to_string(),
            "ALPHA".to_string(),
            " ".to_string(),
            "STREETTYPE".to_string(),
        ];
        let output = extractor
            .parse_tokens(
                "123 MAIN ST",
                &tokens,
                &classes,
                "<<CIVIC#>> <<STREET@>> <<TYPE::STREETTYPE>>",
                MatchMode::Whole,
            )
            .expect("pattern should parse");
        assert_eq!(output.fields.get("CIVIC").map(String::as_str), Some("123"));
        assert_eq!(
            output.fields.get("STREET").map(String::as_str),
            Some("MAIN")
        );
        assert_eq!(output.fields.get("TYPE").map(String::as_str), Some("ST"));
        assert_eq!(output.complement, "");
    }

    #[test]
    fn test_class_comparator_filters_expected_groups() {
        let extractor = mock_extractor();
        let compiled = extractor
            .compile_pattern("<<CIVIC#>> <<STREET@>> <<TYPE>>")
            .expect("pattern should parse");
        let class_pattern = compiled.class_pattern(MatchMode::Whole).to_string();
        let class_regex = compiled
            .class_regex(MatchMode::Whole)
            .expect("class regex compiles");
        let captures = class_regex
            .captures(b"NUM ALPHA STREETTYPE")
            .expect("class regex runs")
            .unwrap_or_else(|| panic!("class comparator should match: {class_pattern}"));
        let groups = capture_groups(&captures);
        let filtered = filter_class_groups(&groups, compiled.class_segments());

        assert_eq!(
            groups,
            vec![
                Some("NUM".to_string()),
                Some("ALPHA".to_string()),
                Some("STREETTYPE".to_string()),
            ]
        );
        assert_eq!(
            filtered,
            Some(vec![
                Some("NUM".to_string()),
                Some("ALPHA".to_string()),
                Some("STREETTYPE".to_string()),
            ])
        );
    }

    #[test]
    fn test_parse_tokens_matches_python_like_class_filter_case() {
        let extractor = mock_extractor();
        let tokens = vec!["TEST".to_string()];
        let classes = vec!["ALPHA".to_string()];
        let output = extractor
            .parse_tokens(
                "TEST",
                &tokens,
                &classes,
                "<<VAR[ALPHA|NUM]>>",
                MatchMode::Whole,
            )
            .expect("pattern should parse");

        assert_eq!(output.fields.get("VAR").map(String::as_str), Some("TEST"));
        assert_eq!(output.complement, "");
    }

    #[test]
    fn test_parse_tokens_matches_python_like_optional_prefix_case() {
        let extractor = mock_extractor();
        let tokens = vec!["NS".to_string()];
        let classes = vec!["PROV".to_string()];
        let output = extractor
            .parse_tokens(
                "NS",
                &tokens,
                &classes,
                "<<MUN@+?#>> <<PROV::PROV>>",
                MatchMode::Whole,
            )
            .expect("pattern should parse");

        assert_eq!(output.fields.get("PROV").map(String::as_str), Some("NS"));
        assert_eq!(output.fields.get("MUN"), None);
        assert_eq!(output.complement, "");
    }

    #[test]
    fn test_parse_tokens_matches_python_like_start_mode_prefix_case() {
        let extractor = mock_extractor();
        let tokens = vec![
            "123".to_string(),
            " ".to_string(),
            "MAIN".to_string(),
            " ".to_string(),
            "ST".to_string(),
            " ".to_string(),
            "EXTRA".to_string(),
        ];
        let classes = vec![
            "NUM".to_string(),
            " ".to_string(),
            "ALPHA".to_string(),
            " ".to_string(),
            "ALPHA".to_string(),
            " ".to_string(),
            "ALPHA".to_string(),
        ];
        let output = extractor
            .parse_tokens(
                "123 MAIN ST EXTRA",
                &tokens,
                &classes,
                "<<CIVIC#>> <<STREET@>>",
                MatchMode::Start,
            )
            .expect("pattern should parse");

        assert_eq!(output.fields.get("CIVIC").map(String::as_str), Some("123"));
        assert_eq!(
            output.fields.get("STREET").map(String::as_str),
            Some("MAIN")
        );
        assert_eq!(output.complement, "ST EXTRA");
    }
}
