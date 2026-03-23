# tokmat 0.2.0

`tokmat` 0.2.0 focuses on productionizing the standalone parser kernel for
reuse by the native Polars integration layer while keeping the crate itself
publishable as a clean Rust dependency.

## Highlights

- Borrowed token/class extraction entry points for callers that already have
  token and class views
- Reduced temporary allocation pressure in the extractor hot path
- Better extractor profiling counters for class regex, fallback regex, and
  offset/object work
- Release QA tightened so formatting, Clippy, tests, docs, and release builds
  are all part of the release-prep gate

## Why this release matters

The main practical improvement in this release is that `tokmat` is easier to
embed in higher-level native pipelines without forcing every caller to rebuild
owned token/class strings up front. That keeps the crate aligned with the
larger goal of using it as the reusable parser kernel while preserving a clean
standalone API.

## Validation

The release candidate was validated with:

- `cargo fmt --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all-targets --all-features`
- `cargo test --doc --all-features`
- `cargo check --release --all-features`
- `cargo doc --no-deps --all-features`

## Scope

`tokmat` remains intentionally focused on:

- normalization
- tokenization and classification
- TEL compilation
- extraction over token classes

It does not attempt to own higher-level strategy orchestration or Python/Polars
distribution concerns.
