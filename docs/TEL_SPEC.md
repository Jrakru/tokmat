# TEL Specification

## Context

TEL is the typed pattern language used by `tokmat` to extract structured fields from a tokenized,
classified input stream.

It is designed to work after tokenization and classification, not as a replacement for them.

## Mental model

Think of TEL as matching a sequence of token descriptors:

```text
Raw text  ->  token boundaries  ->  token types/classes  ->  TEL extraction
```

A TEL rule should answer:

- Which token shapes are required?
- Which semantic classes are required?
- Which parts should be captured?
- Which parts should be ignored or vanish from the result?

## Syntax

### Capturing groups

Capture a field by name:

```text
<<CITY>>
```

### Type modifiers

Apply token-shape constraints:

```text
<<CIVIC#>>
<<STREET@>>
<<UNIT@%>>
```

Meaning:

- `@` alpha-oriented token constraint
- `#` numeric token constraint
- `%` extended token forms
- `=` strict class handling

### Quantifiers

Control cardinality:

```text
<<NAME@+>>
<<SUFFIX@?>>
```

Meaning:

- `+` one or more
- `?` optional

### Greedy matching

Use `$` when a segment should absorb more of the eligible sequence:

```text
<<CITY@+$>>
```

### Explicit classes

Require a token to belong to a semantic class:

```text
<<TYPE::STREETTYPE>>
<<PROV::PROV>>
<<PC::PCODE>>
```

### Literal blocks

Match a literal phrase as a unit:

```text
{{PO BOX}}
```

### Vanishing groups

Require a token or class to appear without returning it as a captured field:

```text
<!PROV!>
```

## Examples

### Street address

```text
<<CIVIC#>> <<STREET@+>> <<TYPE::STREETTYPE>>
```

Possible match:

```text
123 MAIN ST
```

Result:

```text
CIVIC = 123
STREET = MAIN
TYPE = ST
```

### PO Box

```text
{{PO BOX}} <<BOXNUM#>>
```

Possible match:

```text
PO BOX 99
```

Result:

```text
BOXNUM = 99
```

### City, province, postal code

```text
<<CITY@+$>> <<PROV::PROV>> <<PC::PCODE>>
```

Possible match:

```text
OTTAWA ON K1A0B1
```

Result:

```text
CITY = OTTAWA
PROV = ON
PC = K1A0B1
```

## Two-phase example

### Input

```text
123 MAIN ST
```

### Tokenization

```text
["123", " ", "MAIN", " ", "ST"]
```

### Type/class assignment

```text
["NUM", " ", "ALPHA", " ", "STREETTYPE"]
```

### TEL rule

```text
<<CIVIC#>> <<NAME@+>> <<TYPE::STREETTYPE>>
```

### Why this is metadata-driven

The last token is accepted because it belongs to the `STREETTYPE` class, not because the parser
has a hard-coded special case for the literal string `ST`.

That distinction is what makes TEL reusable across different token models.

## Practical guidance

- Use literal blocks for true fixed phrases.
- Use explicit semantic classes when the token meaning matters more than the literal spelling.
- Use `+` for multi-token street or city names.
- Keep TEL focused on extraction semantics and let tokenization solve boundary issues first.
- If you reuse the same TEL rules at high volume, prefer compiling them once and reusing the
  compiled form rather than recompiling under cache pressure.

## Source of truth

For the machine-oriented grammar, see `grammar/tel.ebnf`.
