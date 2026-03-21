# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog and this project follows Semantic Versioning.

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
