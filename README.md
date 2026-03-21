# tokmat

[![CI](https://github.com/Jrakru/tokmat/actions/workflows/ci.yml/badge.svg)](https://github.com/Jrakru/tokmat/actions/workflows/ci.yml)
[![docs.rs](https://docs.rs/tokmat/badge.svg)](https://docs.rs/tokmat)
[![crates.io](https://img.shields.io/crates/v/tokmat.svg)](https://crates.io/crates/tokmat)

`tokmat` is a standalone Rust crate for metadata-driven tokenization and TEL-based extraction of
Canadian-style address strings.

It is the low-level parsing core: other crates can build strategies, pipelines, analytics, or
language bindings on top of it without pulling in broader workspace assumptions.

`tokmat` now uses PCRE2 as its runtime regex engine across tokenization, TEL compilation, and
extractor execution.

## Highlights

- Standalone core crate with no sibling-workspace runtime assumptions
- PCRE2-only runtime regex path across tokenization and extraction
- Metadata-driven TEL extraction over token classes instead of raw-text-only matching
- File-backed token models plus inline/in-memory model support
- Reference corpus tests, doctests, linting, and publish dry-run validation

## Why this crate exists

`tokmat` separates address parsing into two explicit phases:

1. Tokenization and classification
2. TEL-driven extraction over token classes

That split keeps the parser predictable.

- Tokenization decides where boundaries are.
- Classification decides what each token is.
- TEL decides which token-class sequence to match and what to capture.

This is a better fit for messy address data than pushing everything into one monolithic regex.

## Parsing model

```text
Raw input
  |
  v
+---------------------------+
| normalize / clean input   |
+---------------------------+
  |
  v
+---------------------------+
| tokenize into boundaries  |
| ex: ["123", " ", "MAIN"]  |
+---------------------------+
  |
  v
+---------------------------+
| classify each token       |
| ex: ["NUM", " ", "ALPHA"] |
+---------------------------+
  |
  v
+---------------------------+
| compile TEL pattern       |
| ex: <<NUM#>> <<NAME@+>>   |
+---------------------------+
  |
  v
+---------------------------+
| match on class stream     |
| capture named fields      |
+---------------------------+
```

The important design point is that TEL operates over token metadata, not only raw characters.

## Extractor entry modes

The extractor exposes two ways to run TEL:

- `parse_tokens(...)`
- `compile_pattern(...)` + `parse_compiled_tokens(...)`

They are not two different extractors. They are two entry points into the same extractor runtime.

```text
Compat path
pattern string
  -> compile or fetch compiled TEL pattern
  -> build/fetch object plan
  -> run extractor

Precompiled path
compiled pattern
  -> build/fetch object plan
  -> run extractor
```

### When to use each

Use `parse_tokens(...)` when:

- you want the simplest API
- patterns are dynamic or user-supplied
- you are fine relying on the internal compiled-pattern cache

Use `compile_pattern(...)` + `parse_compiled_tokens(...)` when:

- you load a fixed TEL set once and reuse it many times
- you want TEL validation to happen up front
- you expect high pattern churn or a tiny compiled-pattern cache

### Which API should I call?

Use this rule of thumb:

```text
Do you already have a compiled TEL set that will be reused?
  |
  +-- no  -> use parse_tokens(...)
  |
  +-- yes -> use parse_compiled_tokens(...)
```

Another way to say it:

- application code and ad hoc parsing usually want `parse_tokens(...)`
- long-lived workers, services, and batch pipelines usually want precompiled TEL patterns

### Why they can benchmark the same

On the reference corpus used by this crate:

- `695` extractor cases
- `344` unique TEL patterns
- default compiled-pattern cache capacity: `512`

That means the compat path quickly warms the cache and then behaves almost like the precompiled
path. In the 10MM volume benchmark the two extractor modes were effectively identical:

```text
10MM operations, default cache sizes

extractor-compat      30,407 ops/s   16.8 MB RSS
extractor-precompiled 30,127 ops/s   16.2 MB RSS
```

That result does not mean precompiled mode is useless. It means the current corpus is
cache-friendly.

### When precompiled actually matters

Under cache pressure, precompiled mode separates clearly. With the compiled-pattern cache forced to
capacity `1`:

```text
1MM operations, compiled-pattern cache = 1

extractor-compat      12,828 ops/s    7.2 MB RSS
extractor-precompiled 30,609 ops/s   12.5 MB RSS

precompiled vs compat: 2.386x faster
```

Interpretation:

- `compat` is the convenience API
- `precompiled` is the explicit reuse API
- on cache-friendly workloads they converge
- on churn-heavy workloads precompiled mode avoids repeated TEL compilation cost

## TEL in one page

TEL stands for Token Extraction Language.

A TEL pattern is made of typed segments:

- Captures: `<<FIELD>>`
- Captures with type modifiers: `<<STREET@+>>`
- Explicit class constraints: `<<TYPE::STREETTYPE>>`
- Vanishing groups: `<!PROV!>`
- Literal blocks: `{{PO BOX}}`

Common modifiers:

- `@` alpha-like token matching
- `#` numeric token matching
- `%` extended token matching
- `+` one or more
- `?` optional
- `$` greedy matching
- `::CLASSNAME` explicit class assignment

Examples:

- `<<CIVIC#>> <<STREET@+>> <<TYPE::STREETTYPE>>`
- `{{PO BOX}} <<BOXNUM#>>`
- `<<CITY@+$>> <<PROV::PROV>> <<PC::PCODE>>`

See [`docs/TEL_SPEC.md`](docs/TEL_SPEC.md) for a cleaner language reference.

## Quick start

### In-memory token model

This example keeps the model inline so it is easy to understand and compiles without external
files.

```rust
use std::collections::HashSet;

use tokmat::extractor::Extractor;
use tokmat::tokenizer::{tokenize_and_classify, TokenClassList, TokenDefinition};

let token_definitions: TokenDefinition = vec![
    ("NUM".into(), r"\d+".into()),
    ("ALPHA".into(), r"[A-Z]+".into()),
    ("ALPHA_EXTENDED".into(), r"[A-Z][A-Z'\\-]*".into()),
];

let token_class_list: TokenClassList = vec![
    ("STREETTYPE".into(), HashSet::from(["ST".to_string(), "AVE".to_string()])),
];

let tokenized = tokenize_and_classify(
    "123 MAIN ST",
    &token_definitions,
    Some(&token_class_list),
);

assert_eq!(tokenized.tokens, vec!["123", " ", "MAIN", " ", "ST"]);
assert_eq!(tokenized.types[0], "NUM");

let extractor = Extractor::new(token_definitions, token_class_list);
let (_, fields, complement) =
    extractor.parse_string("123 MAIN ST", "<<CIVIC#>> <<NAME@+>> <<TYPE::STREETTYPE>>")?;

assert_eq!(fields.get("CIVIC").map(String::as_str), Some("123"));
assert_eq!(fields.get("NAME").map(String::as_str), Some("MAIN"));
assert_eq!(fields.get("TYPE").map(String::as_str), Some("ST"));
assert_eq!(complement, "");
# Ok::<(), tokmat::error::ParseError>(())
```

### File-backed token model

If you already have a model directory in the wanParser-style layout:

```text
model/
  TOKENDEFINITION/TOKENDEFINITONS.param2
  TOKENCLASS/*.param
```

you can load it directly:

```rust,no_run
use tokmat::extractor::Extractor;
use tokmat::token_model::TokenModel;
use tokmat::tokenizer::tokenize_with_model;

let model = TokenModel::load("tests/fixtures/model_1")?;
let tokenized = tokenize_with_model("123 MAIN ST", &model);

let extractor = Extractor::new(
    model.token_definitions().clone(),
    model.token_class_list().clone(),
);

let (_, fields, _) =
    extractor.parse_string("123 MAIN ST", "<<CIVIC#>> <<NAME@+>> <<TYPE::STREETTYPE>>")?;

assert_eq!(tokenized.tokens[0], "123");
assert_eq!(fields.get("CIVIC").map(String::as_str), Some("123"));
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Two-phase extraction

The crate is easiest to reason about when you think in phases.

### Phase 1: tokenization

Input:

```text
APT-210 O'CONNOR ST
```

Boundary handling preserves address-relevant shapes:

```text
["APT-210", " ", "O'CONNOR", " ", "ST"]
```

This matters because `APT-210` and `O'CONNOR` should not be destroyed by a simplistic
whitespace-only split.

### Phase 2: metadata-driven extraction

Once each token has a type or class, TEL matches over the class sequence rather than blindly over
raw characters.

Example:

```text
Tokens : ["123", " ", "MAIN", " ", "ST"]
Types  : ["NUM", " ", "ALPHA", " ", "ALPHA"]
Class  : ["NUM", " ", "ALPHA", " ", "STREETTYPE"]
TEL    : <<CIVIC#>> <<NAME@+>> <<TYPE::STREETTYPE>>
```

The `TYPE` field is extracted because `ST` is known to belong to the `STREETTYPE` class.

That is the metadata-driven part of the design: the extraction rule is not just matching the text
`"ST"`, it is matching the semantic class attached to that token.

## Benchmarks

The benchmark scripts and JSON artifacts used during crate extraction live in the parent repo:

- `scripts/benchmark_tokmat_variants.py`
- `scripts/benchmark_extractor_mode_tradeoffs.py`

Two benchmark snapshots are especially useful:

### PCRE2-only crate vs earlier mixed-engine crate

```text
10MM operations

tokenizer
  mixed engines : 354,382 ops/s   6.1 MB RSS
  pcre2 only    : 564,171 ops/s   3.6 MB RSS

extractor-compat
  mixed engines : 30,407 ops/s   16.8 MB RSS
  pcre2 only    : 30,435 ops/s   12.6 MB RSS

extractor-precompiled
  mixed engines : 30,127 ops/s   16.2 MB RSS
  pcre2 only    : 30,168 ops/s   12.6 MB RSS
```

Takeaway:

- PCRE2-only materially improves tokenizer throughput
- extractor throughput stays essentially flat
- RSS drops across the measured workloads

### Extractor mode trade-off under cache pressure

```text
1MM operations, compiled-pattern cache = 1

extractor-compat      12,828 ops/s    7.2 MB RSS
extractor-precompiled 30,609 ops/s   12.5 MB RSS
```

Takeaway:

- default corpus + default cache sizes make compat and precompiled look similar
- precompiled mode matters when many pattern compiles would otherwise be repeated
- if you do not know yet, start with `parse_tokens(...)` and only move to precompiled patterns
  when you need explicit reuse or validation

## What makes the crate polished for publication

- Standalone fixture corpus under `tests/`
- Strict linting through Clippy
- Complexity gate validated during development
- Formal TEL grammar in `grammar/tel.ebnf`
- Public docs suitable for crates.io and docs.rs

## Limitations

- The crate is intentionally low-level. It does not try to solve full multi-strategy address
  interpretation by itself.
- TEL is powerful, but it assumes you have a reasonable token model.
- The API focuses on extraction primitives; higher-level strategy orchestration belongs in layers
  above this crate.

## License

MIT. See the `LICENSE` file in the crate root.
