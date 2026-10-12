//! YAML front matter as GitHub shows it: a table of keys and values (docs/plan.md §5).
//!
//! Only flat front matter is read: `key: value` lines, with a value that is a plain or quoted
//! scalar, a flow list (`[a, b]`) or a block list (indented `- item` lines). Anything else
//! (nested maps, multi-line strings, anchors) is shown as the YAML it is; GitHub would show
//! nested tables, which the model does not have.

/// The rows of a flat front matter block (the text between the `---` lines), or `None` if it is
/// not flat.
pub(crate) fn rows(text: &str) -> Option<Vec<(String, String)>> {
    let mut rows: Vec<(String, String)> = Vec::new();
    // The key whose value is a block list being read, and its items.
    let mut list: Option<Vec<String>> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if line.starts_with([' ', '\t', '-']) {
            let item = trimmed
                .strip_prefix("- ")
                .or_else(|| (trimmed == "-").then_some(""))?;
            list.as_mut()?.push(scalar(item)?);
            continue;
        }
        if let Some(items) = list.take() {
            set_last_value(&mut rows, items.join(", "));
        }
        let (key, value) = line.split_once(':')?;
        let value = value.trim();
        if !value.is_empty() && !line.contains(": ") {
            return None; // `a:b` is a scalar, not a key
        }
        let key = scalar(key.trim())?;
        if key.is_empty() {
            return None;
        }
        if value.is_empty() {
            rows.push((key, String::new()));
            list = Some(Vec::new());
        } else if let Some(inner) = value.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
            let items: Option<Vec<String>> = inner
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(scalar)
                .collect();
            rows.push((key, items?.join(", ")));
        } else {
            rows.push((key, scalar(value)?));
        }
    }
    if let Some(items) = list.take() {
        set_last_value(&mut rows, items.join(", "));
    }
    (!rows.is_empty()).then_some(rows)
}

fn set_last_value(rows: &mut [(String, String)], value: String) {
    if let Some((_key, last)) = rows.last_mut() {
        *last = value;
    }
}

/// A plain or quoted scalar's text; `None` for YAML this reader does not take (maps, anchors,
/// tags, block strings).
fn scalar(value: &str) -> Option<String> {
    let value = value.trim();
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return Some(inner.to_owned());
        }
    }
    let special = value.starts_with(['{', '[', '&', '*', '!', '|', '>', '"', '\'', '@', '`']);
    if special {
        return None;
    }
    // A comment after a plain scalar is not part of it.
    let value = value.split(" #").next().unwrap_or(value).trim_end();
    Some(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(rows: &[(&str, &str)]) -> Vec<(String, String)> {
        rows.iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn flat_front_matter_is_rows() {
        assert_eq!(
            rows("title: Front matter test\ntags: [a, b]\n"),
            Some(pairs(&[("title", "Front matter test"), ("tags", "a, b")]))
        );
        assert_eq!(
            rows("# comment\nname: \"quoted: yes\"\nother: 'single' \nlate: x # note\n"),
            Some(pairs(&[
                ("name", "quoted: yes"),
                ("other", "single"),
                ("late", "x")
            ]))
        );
        assert_eq!(
            rows("tags:\n  - one\n  - 'two'\nempty:\nnext: z\n"),
            Some(pairs(&[("tags", "one, two"), ("empty", ""), ("next", "z")]))
        );
        assert_eq!(rows("list:\n- a\n- b"), Some(pairs(&[("list", "a, b")])));
    }

    #[test]
    fn anything_else_is_not_rows() {
        assert_eq!(rows(""), None);
        assert_eq!(rows("just text"), None);
        assert_eq!(rows("nested:\n  key: value\n"), None);
        assert_eq!(rows("text: |\n  block\n"), None);
        assert_eq!(rows("map: {a: 1}\n"), None);
        assert_eq!(rows("url:http://x\n"), None);
        assert_eq!(rows(": no key\n"), None);
        assert_eq!(rows("  - orphan item\n"), None);
        assert_eq!(rows("a: [x, {y: 1}]\n"), None);
    }
}
