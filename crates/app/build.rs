//! Builds the viewer's syntax grammars (docs/plan.md §7, "Licences"): two-face's set (bat's
//! grammars) without those in `grammars-excluded.txt`, whose licences do not allow shipping them
//! in this binary. two-face is a build and test dependency only, so its own set is never linked.
//!
//! A built set refers to other grammars' contexts by index, so leaving grammars out renumbers
//! every reference after them (through serde: syntect's indices are private). A reference into
//! a grammar left out goes to a stand-in (`STAND_INS`, else Plain Text). The tests in
//! `highlighting.rs` check every grammar kept highlights as two-face's does.

#![expect(
    clippy::expect_used,
    reason = "a build script fails the build by panicking, with the reason"
)]

use std::collections::HashMap;
use std::path::PathBuf;

use syntect::parsing::{SyntaxDefinition, SyntaxSetBuilder};

/// Grammars left out that others embed, and what they get instead.
const STAND_INS: [(&str, &str); 1] = [("Less", "CSS")];
const FALLBACK: &str = "Plain Text";

fn main() {
    println!("cargo::rerun-if-changed=grammars-excluded.txt");
    println!("cargo::rerun-if-changed=build.rs");
    let excluded: Vec<&str> = include_str!("grammars-excluded.txt")
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .filter_map(|line| line.split('\t').next())
        .collect();
    let set = two_face::syntax::extra_newlines();
    let names: Vec<String> = set.syntaxes().iter().map(|s| s.name.clone()).collect();
    for name in &excluded {
        assert!(
            names.iter().any(|n| n == name),
            "excluded grammar not in two-face: {name}"
        );
    }
    // Each grammar's index in the new set, if kept.
    let mut kept = 0;
    let index: Vec<Option<usize>> = names
        .iter()
        .map(|name| {
            (!excluded.contains(&name.as_str())).then(|| {
                kept += 1;
                kept - 1
            })
        })
        .collect();
    let definitions = set.into_builder().syntaxes().to_vec();
    // A stand-in's main context: contexts are numbered in name order within a grammar.
    let main_of = |name: &str| -> (usize, usize) {
        let old = names
            .iter()
            .position(|n| n == name)
            .expect("stand-in grammar");
        let definition = definitions.get(old).expect("its definition");
        let mut contexts: Vec<&String> = definition.contexts.keys().collect();
        contexts.sort();
        let main = contexts
            .iter()
            .position(|c| *c == "main")
            .expect("a main context");
        (
            index.get(old).copied().flatten().expect("stand-in kept"),
            main,
        )
    };
    let stand_ins: HashMap<usize, (usize, usize)> = names
        .iter()
        .enumerate()
        .filter(|(_, name)| excluded.contains(&name.as_str()))
        .map(|(old, name)| {
            let to = STAND_INS
                .iter()
                .find(|(from, _)| from == name)
                .map_or(FALLBACK, |(_, to)| to);
            (old, main_of(to))
        })
        .collect();
    let mut builder = SyntaxSetBuilder::new();
    for (old, definition) in definitions.iter().enumerate() {
        if index.get(old).copied().flatten().is_none() {
            continue;
        }
        let mut value = serde_json::to_value(definition).expect("a definition serializes");
        renumber(&mut value, &index, &stand_ins);
        builder.add(serde_json::from_value::<SyntaxDefinition>(value).expect("and back"));
    }
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    syntect::dumps::dump_to_uncompressed_file(&builder.build(), out.join("syntaxes.packdump"))
        .expect("the set is written");
    // The kept grammars' licence notices (bat's listing: grammars left out have none in it),
    // for scripts/third-party to put in THIRD-PARTY-NOTICES.md.
    std::fs::write(
        out.join("grammar-notices.md"),
        two_face::acknowledgement::listing().to_md(),
    )
    .expect("the notices are written");
}

/// Renumbers every context reference (`{syntax_index, context_index}`) in `value`: to the
/// grammar's new index, or to the stand-in for a grammar left out.
fn renumber(
    value: &mut serde_json::Value,
    index: &[Option<usize>],
    stand_ins: &HashMap<usize, (usize, usize)>,
) {
    match value {
        serde_json::Value::Object(object) => {
            let is_id = object.len() == 2
                && object.contains_key("syntax_index")
                && object.contains_key("context_index");
            if !is_id {
                for item in object.values_mut() {
                    renumber(item, index, stand_ins);
                }
                return;
            }
            let old = object
                .get("syntax_index")
                .and_then(serde_json::Value::as_u64)
                .and_then(|old| usize::try_from(old).ok())
                .expect("an index");
            if let Some(new) = index.get(old).copied().flatten() {
                object.insert("syntax_index".to_owned(), new.into());
            } else {
                let (syntax, context) = stand_ins.get(&old).copied().expect("a stand-in");
                object.insert("syntax_index".to_owned(), syntax.into());
                object.insert("context_index".to_owned(), context.into());
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                renumber(item, index, stand_ins);
            }
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => {}
    }
}
