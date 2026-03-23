# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog and this project follows Semantic Versioning.

## [0.2.0] - 2026-03-23

### Added

- Borrowed token/class extraction entry points for pre-tokenized callers that already hold views
- Extractor profiling counters for class regex, fallback regex, offset work, and object join timing
- Release automation metadata and workflow hardening suitable for tagged crates.io publication

### Changed

- Optimized the extractor hot path to reduce unnecessary object-string reconstruction and span reassembly work
- Improved direct object-plan execution to slice captured values from precomputed spans instead of rejoining token fragments
- Corrected crate repository metadata to point at the standalone `tokmat` repository

### Verified

- `cargo fmt --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all-targets --all-features`
- `cargo test --doc --all-features`
- `cargo check --release --all-features`
- `cargo doc --no-deps --all-features`

## [0.1.0] - 2026-03-21

### Added

- Standalone `tokmat` crate extracted as its own publishable repository
- PCRE2-only runtime regex path across tokenization, TEL compilation, and extraction
- Human-readable TEL reference in `docs/TEL_SPEC.md`
- Reference corpus fixtures and regression tests packaged with the crate
- Benchmark runner example and benchmark-backed extractor API guidance

### Changed

- Clarified extractor entry modes:
  - `parse_tokens(...)` as the convenience API
  - `compile_pattern(...)` + `parse_compiled_tokens(...)` as the explicit reuse API
- Refactored TEL token splitting for clearer internal structure while preserving behavior

### Verified

- `cargo test`
- `cargo doc --no-deps`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo publish --dry-run`
