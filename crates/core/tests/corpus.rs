//! Fidelity oracle 3 (docs/plan.md §9): GitHub's own rendering of every corpus README.
//!
//! Each README's model and GitHub's HTML are reduced to styled characters (see `common`) and
//! compared: the text, then each character's style and link target, then the images. Known
//! differences are listed in `tests/data/corpus-deviations.txt` as substitutions (GitHub's text,
//! ours, and why); the list is exact both ways, so a substitution that no longer applies fails.

mod common;

#[cfg(test)]
mod corpus {
    use std::path::{Path, PathBuf};

    use super::common::{StyledChar, View, first_difference, html_view, model_view, text};

    /// "GitHub shows `github` where we show `ours`", in one corpus file.
    struct Deviation {
        dir: String,
        github: Vec<char>,
        ours: Vec<char>,
    }

    /// `dir<TAB>github<TAB>ours<TAB>reason` per line, in the characters `common::text` shows
    /// (`⏎` a hard break, `▣` an image); `#` comments.
    fn deviations() -> Vec<Deviation> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/corpus-deviations.txt");
        let text = std::fs::read_to_string(path).expect("deviations file");
        text.lines()
            .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
            .map(|line| {
                let fields: Vec<&str> = line.split('\t').collect();
                let [dir, github, ours, reason] = fields.as_slice() else {
                    panic!("deviation line needs 4 tab-separated fields: {line:?}");
                };
                assert!(
                    !reason.trim().is_empty(),
                    "deviation without a reason: {line:?}"
                );
                Deviation {
                    dir: (*dir).to_owned(),
                    github: github.chars().collect(),
                    ours: ours.chars().collect(),
                }
            })
            .collect()
    }

    fn corpus_dirs() -> Vec<PathBuf> {
        let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/corpus");
        let mut dirs: Vec<_> = std::fs::read_dir(corpus)
            .expect("corpus")
            .map(|entry| entry.expect("entry").path())
            .collect();
        dirs.sort();
        dirs
    }

    /// The characters a displayed text stands for (`⏎`, `▣` back to what `text` replaced).
    fn display_char(c: &StyledChar) -> char {
        text(std::slice::from_ref(c)).chars().next().unwrap_or(c.c)
    }

    /// Applies a deviation to GitHub's characters: each occurrence of its text becomes ours,
    /// marked so its styles are not compared. Returns how many occurrences there were.
    fn substitute(chars: &mut Vec<(StyledChar, bool)>, deviation: &Deviation) -> usize {
        let mut count = 0;
        let mut at = 0;
        while at + deviation.github.len() <= chars.len() {
            let matches = chars
                .get(at..at + deviation.github.len())
                .is_some_and(|window| {
                    window
                        .iter()
                        .map(|(c, _)| display_char(c))
                        .eq(deviation.github.iter().copied())
                });
            if matches && !deviation.github.is_empty() {
                let replacement = deviation.ours.iter().map(|&c| {
                    let c = match c {
                        '⏎' => '\u{2}',
                        '▣' => '\u{FFFC}',
                        c => c,
                    };
                    (
                        StyledChar {
                            c,
                            styles: 0,
                            link: None,
                        },
                        true,
                    )
                });
                chars.splice(at..at + deviation.github.len(), replacement);
                at += deviation.ours.len();
                count += 1;
            } else {
                at += 1;
            }
        }
        count
    }

    /// The first difference between GitHub's characters (deviations applied) and ours.
    fn difference(expected: &[(StyledChar, bool)], actual: &View) -> Option<String> {
        let expected_text: String = expected.iter().map(|(c, _)| display_char(c)).collect();
        let actual_text = text(&actual.chars);
        let context = |text: &str, at: usize| -> String {
            text.chars().skip(at.saturating_sub(60)).take(140).collect()
        };
        if expected_text != actual_text {
            let at = expected_text
                .chars()
                .zip(actual_text.chars())
                .position(|(e, a)| e != a)
                .unwrap_or_else(|| {
                    expected_text
                        .chars()
                        .count()
                        .min(actual_text.chars().count())
                });
            return Some(format!(
                "text differs at char {at}:\n    GitHub {:?}\n    ours   {:?}",
                context(&expected_text, at),
                context(&actual_text, at)
            ));
        }
        let at = expected
            .iter()
            .zip(&actual.chars)
            .position(|((e, substituted), a)| !substituted && e != a)?;
        let (e, _) = expected.get(at)?;
        let a = actual.chars.get(at)?;
        Some(format!(
            "styles or link differ at char {at} ({:?}): GitHub {:05b} {:?}, ours {:05b} {:?}\n    near {:?}",
            e.c,
            e.styles,
            e.link,
            a.styles,
            a.link,
            context(&expected_text, at)
        ))
    }

    /// Only the images of a view, as (alt, source), with the rewrites GitHub makes to sources undone: a `/blob/`
    /// URL on github.com is served from `/raw/`, and an uploaded asset
    /// (`github.com/user-attachments/assets/<id>`) from a signed `private-user-images` URL
    /// ending in `-<id>.<ext>`.
    fn images(view: &View) -> View {
        let images = view
            .images
            .iter()
            // GitHub's custom emoji (`:octocat:`) are images there, text here (a listed
            // deviation).
            .filter(|(_alt, source)| !source.contains("githubassets.com/images/icons/emoji/"))
            .map(|(alt, source)| {
                let source = if let Some(id) = source.split("/user-attachments/assets/").nth(1) {
                    format!("asset:{id}")
                } else if source.contains("private-user-images.githubusercontent.com/") {
                    let file = source.rsplit('/').next().unwrap_or_default();
                    let stem = file.rsplit_once('.').map_or(file, |(stem, _ext)| stem);
                    let id = stem.get(stem.len().saturating_sub(36)..).unwrap_or(stem);
                    format!("asset:{id}")
                } else if source.starts_with("https://github.com/") {
                    source.replacen("/blob/", "/raw/", 1)
                } else {
                    source.clone()
                };
                (alt.clone(), source)
            })
            .collect();
        View {
            blocks: Vec::new(),
            chars: Vec::new(),
            images,
        }
    }

    #[test]
    fn the_corpus_matches_github_except_known_deviations() {
        let deviations = deviations();
        let mut failures = Vec::new();
        let mut compared_files = 0;
        let mut compared_chars = 0;
        for dir in corpus_dirs() {
            let name = dir
                .file_name()
                .expect("dir name")
                .to_string_lossy()
                .into_owned();
            let markdown = std::fs::read(dir.join("input.md")).expect("input");
            let github = std::fs::read_to_string(dir.join("github.html")).expect("rendering");
            let expected_view = html_view(&github);
            let actual = model_view(&galley_core::load(&markdown));
            let mut expected: Vec<(StyledChar, bool)> = expected_view
                .chars
                .iter()
                .map(|c| (c.clone(), false))
                .collect();
            for deviation in deviations.iter().filter(|d| d.dir == name) {
                if substitute(&mut expected, deviation) == 0 {
                    failures.push(format!(
                        "{name}: listed deviation no longer applies (remove it): {:?}",
                        deviation.github.iter().collect::<String>()
                    ));
                }
            }
            if let Some(difference) = difference(&expected, &actual) {
                failures.push(format!("{name}: {difference}"));
            } else if let Some(difference) =
                first_difference(&images(&expected_view), &images(&actual))
            {
                failures.push(format!("{name}: {difference}"));
            }
            compared_files += 1;
            compared_chars += actual.chars.len();
        }
        // Count floors: a reduction that finds nothing must not pass.
        assert!(compared_files >= 23, "only {compared_files} corpus files");
        assert!(
            compared_chars >= 200_000,
            "only {compared_chars} characters compared"
        );
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}
