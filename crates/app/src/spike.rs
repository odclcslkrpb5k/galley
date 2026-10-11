//! M0 spike content: the file's text cut into paragraphs at blank lines.
//!
//! Stands in for core's document model until M1 (docs/plan.md §10).

pub(crate) fn paragraphs(text: &str) -> Vec<String> {
    let mut paragraphs = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            if !current.is_empty() {
                paragraphs.push(std::mem::take(&mut current));
            }
        } else {
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(line);
        }
    }
    if !current.is_empty() {
        paragraphs.push(current);
    }
    paragraphs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_lines_separate_paragraphs() {
        assert_eq!(
            paragraphs("# A\n\none\ntwo\n  \n\nthree"),
            vec!["# A", "one\ntwo", "three"]
        );
    }

    #[test]
    fn no_text_no_paragraphs() {
        assert_eq!(paragraphs(""), Vec::<String>::new());
        assert_eq!(paragraphs("\n \n"), Vec::<String>::new());
    }
}
