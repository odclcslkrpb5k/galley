//! Fidelity oracles 1 and 2 (docs/plan.md §9) over the vendored GFM spec and extension tests:
//! for every example, the model's blocks and styled text must match the expected HTML's, except
//! for the examples listed in `tests/data/spec/deviations.txt`, each with a reason.
//!
//! The list is exact both ways: an example that differs and is not listed fails, and a listed
//! example that no longer differs fails too (remove it from the list).

mod common;

#[cfg(test)]
mod spec {
    use std::collections::BTreeMap;
    use std::path::Path;

    use super::common::{first_difference, html_view, model_view};

    struct Example {
        id: String,
        markdown: String,
        html: String,
    }

    /// Reads the examples of a spec-format file, as `<prefix>:<number>`. Tabs are written `→`.
    fn examples(file: &str, prefix: &str) -> Vec<Example> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data/spec")
            .join(file);
        let text = std::fs::read_to_string(&path).expect("vendored spec file");
        let fence = "````````````````````````````````";
        let mut examples = Vec::new();
        let mut lines = text.lines();
        while let Some(line) = lines.next() {
            // Openers may carry a label: "example table", "example disabled", ...
            let is_opener = line
                .strip_prefix(fence)
                .is_some_and(|rest| rest.trim_start().starts_with("example"));
            if !is_opener {
                continue;
            }
            let mut markdown = String::new();
            let mut html = String::new();
            let mut in_html = false;
            for line in lines.by_ref() {
                if line == fence {
                    break;
                }
                if line == "." && !in_html {
                    in_html = true;
                    continue;
                }
                let target = if in_html { &mut html } else { &mut markdown };
                target.push_str(&line.replace('→', "\t"));
                target.push('\n');
            }
            examples.push(Example {
                id: format!("{prefix}:{}", examples.len() + 1),
                markdown,
                html,
            });
        }
        examples
    }

    /// Known deviations: `id reason...` per line; `#` comments.
    fn deviations() -> BTreeMap<String, String> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/spec/deviations.txt");
        let text = std::fs::read_to_string(&path).expect("deviations file");
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|line| {
                let (id, reason) = line.split_once(' ').unwrap_or((line, ""));
                (id.to_owned(), reason.trim().to_owned())
            })
            .collect()
    }

    #[test]
    fn spec_and_extensions_match_except_known_deviations() {
        let spec = examples("spec.txt", "spec");
        let extensions = examples("extensions.txt", "ext");
        // Count floors: a parser change that finds fewer examples must not pass quietly.
        assert_eq!(spec.len(), 672, "GFM spec 0.29 has 672 examples");
        assert_eq!(extensions.len(), 30, "extensions.txt has 30 examples");

        let known = deviations();
        for (id, reason) in &known {
            assert!(!reason.is_empty(), "deviation {id} has no reason");
        }
        let mut unexpected = Vec::new();
        let mut differing = Vec::new();
        for example in spec.iter().chain(&extensions) {
            let doc = gbk_markdown_core::load(example.markdown.as_bytes());
            // "<IGNORE>": the example only checks that parsing does not crash.
            if example.html.trim() == "<IGNORE>" {
                continue;
            }
            let difference = first_difference(&html_view(&example.html), &model_view(&doc));
            if let Some(difference) = difference {
                differing.push(example.id.clone());
                if !known.contains_key(&example.id) {
                    unexpected.push(format!(
                        "{}: {difference}\n    markdown: {:?}",
                        example.id, example.markdown
                    ));
                }
            }
        }
        let stale: Vec<_> = known
            .keys()
            .filter(|id| !differing.contains(id))
            .cloned()
            .collect();
        assert!(
            unexpected.is_empty() && stale.is_empty(),
            "{} unexpected differences:\n{}\n\nlisted deviations that now match (remove them): {stale:?}",
            unexpected.len(),
            unexpected.join("\n"),
        );
    }
}
