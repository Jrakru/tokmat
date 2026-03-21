use std::fs;
use std::path::Path;

use tokmat::tel::CompiledPattern;

fn grammar_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("grammar/tel.ebnf")
}

#[test]
fn test_tel_grammar_declares_required_productions() {
    let grammar = fs::read_to_string(grammar_path()).expect("grammar file should exist");

    for production in [
        "pattern         =",
        "segment         =",
        "capture         =",
        "non_capture     =",
        "vanishing       =",
        "literal         =",
        "class_spec      =",
        "modifier_chain  =",
    ] {
        assert!(
            grammar.contains(production),
            "missing production '{production}' in tel.ebnf"
        );
    }
}

#[test]
fn test_tel_grammar_examples_compile() {
    let grammar = fs::read_to_string(grammar_path()).expect("grammar file should exist");

    let examples: Vec<&str> = grammar
        .lines()
        .filter_map(|line| line.trim().strip_prefix("# Example: "))
        .collect();

    assert!(
        !examples.is_empty(),
        "grammar file should include example patterns"
    );

    for example in examples {
        CompiledPattern::compile(example).unwrap_or_else(|error| {
            panic!("grammar example failed to compile '{example}': {error}")
        });
    }
}
