use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tokmat::extractor::{CacheStats, Extractor, ExtractorConfig, ExtractorStats, MatchMode};
use tokmat::tel::CompiledPattern;
use tokmat::token_model::TokenModel;
use tokmat::tokenizer::tokenize_with_model;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Workload {
    Tokenizer,
    ExtractorCompat,
    ExtractorPrecompiled,
}

#[derive(Debug)]
struct Args {
    workload: Workload,
    fixture: PathBuf,
    model_base: PathBuf,
    iterations: usize,
    warmup: usize,
    compiled_pattern_cache_capacity: usize,
    object_plan_cache_capacity: usize,
    fallback_regex_cache_capacity: usize,
}

#[derive(Debug, Deserialize)]
struct TokenizerCase {
    cleaned: String,
}

#[derive(Debug, Deserialize)]
struct ExtractorCase {
    input: String,
    tokens: Vec<String>,
    classes: Vec<String>,
    pattern: String,
    #[serde(default = "default_mode")]
    mode: String,
}

#[derive(Debug, Serialize)]
struct BenchmarkResult {
    engine: &'static str,
    workload: &'static str,
    fixture: String,
    cases: usize,
    iterations: usize,
    warmup: usize,
    elapsed_seconds: f64,
    operations: usize,
    ops_per_second: f64,
    checksum: u64,
    extractor_stats: Option<ExtractorStatsReport>,
}

#[derive(Debug, Serialize)]
struct ExtractorStatsReport {
    compiled_pattern_cache: CacheStatsReport,
    object_plan_cache: CacheStatsReport,
    fallback_regex_cache: CacheStatsReport,
    unique_plan_signature_count: usize,
    direct_only_plan_count: usize,
    with_fallback_plan_count: usize,
    total_plan_steps: usize,
    single_token_step_count: usize,
    captured_span_step_count: usize,
    literal_step_count: usize,
    direct_execution_attempts: usize,
    direct_execution_hits: usize,
    direct_execution_hit_rate: f64,
    fallback_execution_count: usize,
    fallback_regex_realizations: usize,
}

#[derive(Debug, Serialize)]
struct CacheStatsReport {
    capacity: usize,
    len: usize,
    hits: usize,
    misses: usize,
    inserts: usize,
    evictions: usize,
}

fn main() -> Result<()> {
    let args = parse_args()?;
    let model = TokenModel::load(&args.model_base).with_context(|| {
        format!(
            "failed to load token model from {}",
            args.model_base.display()
        )
    })?;
    let extractor = Extractor::new_with_config(
        model.token_definitions().clone(),
        model.token_class_list().clone(),
        ExtractorConfig {
            compiled_pattern_cache_capacity: args.compiled_pattern_cache_capacity,
            object_plan_cache_capacity: args.object_plan_cache_capacity,
            fallback_regex_cache_capacity: args.fallback_regex_cache_capacity,
        },
    );

    let result = match args.workload {
        Workload::Tokenizer => run_tokenizer(&args, &model)?,
        Workload::ExtractorCompat => run_extractor_compat(&args, &extractor)?,
        Workload::ExtractorPrecompiled => run_extractor_precompiled(&args, &extractor)?,
    };

    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

fn run_tokenizer(args: &Args, model: &TokenModel) -> Result<BenchmarkResult> {
    let cases: Vec<TokenizerCase> = load_json(&args.fixture)?;
    warmup_tokenizer(args.warmup, &cases, model);

    let started = Instant::now();
    let mut checksum = 0_u64;
    for _ in 0..args.iterations {
        for case in &cases {
            let tokenized = black_box(tokenize_with_model(&case.cleaned, model));
            checksum = checksum
                .wrapping_add(tokenized.tokens.len() as u64)
                .wrapping_add(tokenized.types.len() as u64)
                .wrapping_add(tokenized.classes.len() as u64)
                .wrapping_add(tokenized.raw_value.len() as u64);
        }
    }
    let elapsed = started.elapsed().as_secs_f64();
    let operations = cases.len() * args.iterations;
    Ok(BenchmarkResult {
        engine: "rust",
        workload: "tokenizer",
        fixture: args.fixture.display().to_string(),
        cases: cases.len(),
        iterations: args.iterations,
        warmup: args.warmup,
        elapsed_seconds: elapsed,
        operations,
        ops_per_second: operations_per_second(operations, elapsed)?,
        checksum,
        extractor_stats: None,
    })
}

fn run_extractor_compat(args: &Args, extractor: &Extractor) -> Result<BenchmarkResult> {
    let cases: Vec<ExtractorCase> = load_json(&args.fixture)?;
    warmup_extractor_compat(args.warmup, &cases, extractor)?;

    let started = Instant::now();
    let mut checksum = 0_u64;
    for _ in 0..args.iterations {
        for case in &cases {
            let output = black_box(extractor.parse_tokens(
                &case.input,
                &case.tokens,
                &case.classes,
                &case.pattern,
                parse_mode(&case.mode)?,
            )?);
            checksum = checksum
                .wrapping_add(output.fields.len() as u64)
                .wrapping_add(output.complement.len() as u64)
                .wrapping_add(output.uid.len() as u64)
                .wrapping_add(sum_field_lengths(&output.fields));
        }
    }
    let elapsed = started.elapsed().as_secs_f64();
    let operations = cases.len() * args.iterations;
    Ok(BenchmarkResult {
        engine: "rust",
        workload: "extractor_compat",
        fixture: args.fixture.display().to_string(),
        cases: cases.len(),
        iterations: args.iterations,
        warmup: args.warmup,
        elapsed_seconds: elapsed,
        operations,
        ops_per_second: operations_per_second(operations, elapsed)?,
        checksum,
        extractor_stats: Some(extractor_stats_report(extractor.stats()?)),
    })
}

fn run_extractor_precompiled(args: &Args, extractor: &Extractor) -> Result<BenchmarkResult> {
    let cases: Vec<ExtractorCase> = load_json(&args.fixture)?;
    let compiled = compile_patterns(&cases, extractor)?;
    warmup_extractor_precompiled(args.warmup, &cases, extractor, &compiled)?;

    let started = Instant::now();
    let mut checksum = 0_u64;
    for _ in 0..args.iterations {
        for case in &cases {
            let pattern = compiled
                .get(case.pattern.as_str())
                .context("missing compiled pattern")?;
            let output = black_box(extractor.parse_compiled_tokens(
                &case.input,
                &case.tokens,
                &case.classes,
                pattern,
                parse_mode(&case.mode)?,
            )?);
            checksum = checksum
                .wrapping_add(output.fields.len() as u64)
                .wrapping_add(output.complement.len() as u64)
                .wrapping_add(output.uid.len() as u64)
                .wrapping_add(sum_field_lengths(&output.fields));
        }
    }
    let elapsed = started.elapsed().as_secs_f64();
    let operations = cases.len() * args.iterations;
    Ok(BenchmarkResult {
        engine: "rust",
        workload: "extractor_precompiled",
        fixture: args.fixture.display().to_string(),
        cases: cases.len(),
        iterations: args.iterations,
        warmup: args.warmup,
        elapsed_seconds: elapsed,
        operations,
        ops_per_second: operations_per_second(operations, elapsed)?,
        checksum,
        extractor_stats: Some(extractor_stats_report(extractor.stats()?)),
    })
}

fn warmup_tokenizer(warmup: usize, cases: &[TokenizerCase], model: &TokenModel) {
    for _ in 0..warmup {
        for case in cases {
            black_box(tokenize_with_model(&case.cleaned, model));
        }
    }
}

fn warmup_extractor_compat(
    warmup: usize,
    cases: &[ExtractorCase],
    extractor: &Extractor,
) -> Result<()> {
    for _ in 0..warmup {
        for case in cases {
            black_box(extractor.parse_tokens(
                &case.input,
                &case.tokens,
                &case.classes,
                &case.pattern,
                parse_mode(&case.mode)?,
            )?);
        }
    }
    Ok(())
}

fn warmup_extractor_precompiled(
    warmup: usize,
    cases: &[ExtractorCase],
    extractor: &Extractor,
    compiled: &HashMap<&str, CompiledPattern>,
) -> Result<()> {
    for _ in 0..warmup {
        for case in cases {
            let pattern = compiled
                .get(case.pattern.as_str())
                .context("missing compiled pattern")?;
            black_box(extractor.parse_compiled_tokens(
                &case.input,
                &case.tokens,
                &case.classes,
                pattern,
                parse_mode(&case.mode)?,
            )?);
        }
    }
    Ok(())
}

fn compile_patterns<'a>(
    cases: &'a [ExtractorCase],
    extractor: &Extractor,
) -> Result<HashMap<&'a str, CompiledPattern>> {
    let mut compiled = HashMap::new();
    for case in cases {
        if compiled.contains_key(case.pattern.as_str()) {
            continue;
        }
        compiled.insert(
            case.pattern.as_str(),
            extractor.compile_pattern(&case.pattern)?,
        );
    }
    Ok(compiled)
}

fn sum_field_lengths(fields: &HashMap<String, String>) -> u64 {
    fields
        .iter()
        .map(|(key, value)| (key.len() + value.len()) as u64)
        .sum()
}

fn load_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read fixture {}", path.display()))?;
    serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse fixture {}", path.display()))
}

fn default_mode() -> String {
    "whole".to_string()
}

fn parse_mode(mode: &str) -> Result<MatchMode> {
    match mode {
        "whole" => Ok(MatchMode::Whole),
        "start" => Ok(MatchMode::Start),
        "end" => Ok(MatchMode::End),
        "any" => Ok(MatchMode::Any),
        _ => bail!("unsupported match mode '{mode}'"),
    }
}

fn parse_args() -> Result<Args> {
    let mut workload = None;
    let mut fixture = None;
    let mut model_base = None;
    let mut iterations = 20_usize;
    let mut warmup = 2_usize;
    let defaults = ExtractorConfig::default();
    let mut compiled_pattern_cache_capacity = defaults.compiled_pattern_cache_capacity;
    let mut object_plan_cache_capacity = defaults.object_plan_cache_capacity;
    let mut fallback_regex_cache_capacity = defaults.fallback_regex_cache_capacity;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--workload" => {
                let value = args.next().context("missing value for --workload")?;
                workload = Some(parse_workload(&value)?);
            }
            "--fixture" => {
                fixture = Some(PathBuf::from(
                    args.next().context("missing value for --fixture")?,
                ));
            }
            "--model-base" => {
                model_base = Some(PathBuf::from(
                    args.next().context("missing value for --model-base")?,
                ));
            }
            "--iterations" => {
                iterations = args
                    .next()
                    .context("missing value for --iterations")?
                    .parse()
                    .context("invalid --iterations value")?;
            }
            "--warmup" => {
                warmup = args
                    .next()
                    .context("missing value for --warmup")?
                    .parse()
                    .context("invalid --warmup value")?;
            }
            "--compiled-pattern-cache-capacity" => {
                compiled_pattern_cache_capacity = args
                    .next()
                    .context("missing value for --compiled-pattern-cache-capacity")?
                    .parse()
                    .context("invalid --compiled-pattern-cache-capacity value")?;
            }
            "--object-plan-cache-capacity" => {
                object_plan_cache_capacity = args
                    .next()
                    .context("missing value for --object-plan-cache-capacity")?
                    .parse()
                    .context("invalid --object-plan-cache-capacity value")?;
            }
            "--fallback-regex-cache-capacity" => {
                fallback_regex_cache_capacity = args
                    .next()
                    .context("missing value for --fallback-regex-cache-capacity")?
                    .parse()
                    .context("invalid --fallback-regex-cache-capacity value")?;
            }
            other => bail!("unknown argument '{other}'"),
        }
    }

    Ok(Args {
        workload: workload.context("missing --workload")?,
        fixture: fixture.context("missing --fixture")?,
        model_base: model_base.context("missing --model-base")?,
        iterations,
        warmup,
        compiled_pattern_cache_capacity,
        object_plan_cache_capacity,
        fallback_regex_cache_capacity,
    })
}

fn parse_workload(value: &str) -> Result<Workload> {
    match value {
        "tokenizer" => Ok(Workload::Tokenizer),
        "extractor-compat" => Ok(Workload::ExtractorCompat),
        "extractor-precompiled" => Ok(Workload::ExtractorPrecompiled),
        _ => bail!("unsupported workload '{value}'"),
    }
}

fn operations_per_second(operations: usize, elapsed: f64) -> Result<f64> {
    let operations = u32::try_from(operations).context("operations exceed u32::MAX")?;
    Ok(f64::from(operations) / elapsed)
}

fn extractor_stats_report(stats: ExtractorStats) -> ExtractorStatsReport {
    let direct_execution_hit_rate = if stats.direct_execution_attempts == 0 {
        0.0
    } else {
        let hits = u32::try_from(stats.direct_execution_hits)
            .expect("benchmark hit count exceeds u32::MAX");
        let attempts = u32::try_from(stats.direct_execution_attempts)
            .expect("benchmark attempt count exceeds u32::MAX");
        f64::from(hits) / f64::from(attempts)
    };
    ExtractorStatsReport {
        compiled_pattern_cache: cache_stats_report(stats.compiled_pattern_cache),
        object_plan_cache: cache_stats_report(stats.object_plan_cache),
        fallback_regex_cache: cache_stats_report(stats.fallback_regex_cache),
        unique_plan_signature_count: stats.unique_plan_signature_count,
        direct_only_plan_count: stats.direct_only_plan_count,
        with_fallback_plan_count: stats.with_fallback_plan_count,
        total_plan_steps: stats.total_plan_steps,
        single_token_step_count: stats.single_token_step_count,
        captured_span_step_count: stats.captured_span_step_count,
        literal_step_count: stats.literal_step_count,
        direct_execution_attempts: stats.direct_execution_attempts,
        direct_execution_hits: stats.direct_execution_hits,
        direct_execution_hit_rate,
        fallback_execution_count: stats.fallback_execution_count,
        fallback_regex_realizations: stats.fallback_regex_realizations,
    }
}

const fn cache_stats_report(stats: CacheStats) -> CacheStatsReport {
    CacheStatsReport {
        capacity: stats.capacity,
        len: stats.len,
        hits: stats.hits,
        misses: stats.misses,
        inserts: stats.inserts,
        evictions: stats.evictions,
    }
}
