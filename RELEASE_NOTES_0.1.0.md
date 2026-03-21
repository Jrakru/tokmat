# tokmat 0.1.0

`tokmat` is the standalone low-level parsing core extracted from the broader wanParser work. This
first crate release focuses on making the tokenizer, TEL compiler, and extractor publishable as a
clean Rust crate with a coherent runtime story and reference-backed docs.

## Highlights

- PCRE2-only runtime regex path across tokenization, TEL compilation, and extraction
- Metadata-driven address parsing pipeline:
  - normalization
  - tokenization and classification
  - TEL compilation
  - extraction over token classes
- Standalone fixture corpus and model fixtures packaged with the crate for reproducible testing
- Human-readable TEL specification alongside the machine grammar
- Benchmarked extractor API guidance for `parse_tokens(...)` vs `parse_compiled_tokens(...)`

## Performance notes

Two benchmark snapshots were used during release preparation:

1. 10MM operation comparison between the earlier mixed-engine variant and the PCRE2-only crate
2. extractor mode trade-off benchmark under forced compiled-pattern cache pressure

Key takeaways:

- moving to a PCRE2-only runtime path improves tokenizer throughput materially
- extractor throughput stays essentially flat on the reference corpus
- precompiled TEL patterns matter most when pattern compilation would otherwise churn

## Packaging and quality gates

The release was validated with:

- `cargo test`
- `cargo doc --no-deps`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo publish --dry-run`

## Scope

`tokmat` is intentionally the parsing core, not the full strategy/orchestration layer. It provides
the primitives other crates can build on for:

- strategy execution
- language bindings
- analytics
- higher-level address parsing pipelines
