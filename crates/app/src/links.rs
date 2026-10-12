//! Where a relative link goes (docs/plan.md §5): resolved against the document's folder, or for
//! `/path` against its repository's root, as GitHub resolves them. Pure: the file system is
//! asked through the closures passed in, so it is unit-tested.

use std::path::{Component, Path, PathBuf};

/// What opening a relative link means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Opening {
    /// A Markdown file: shown in a new viewer window, at the fragment if there is one.
    Viewer {
        path: PathBuf,
        fragment: Option<String>,
    },
    /// Anything else: handed to the system's default application.
    System(PathBuf),
}

/// File name extensions read as Markdown (GitHub's list, less the rare ones).
const MARKDOWN: [&str; 6] = ["md", "markdown", "mdown", "mkdn", "mkd", "mdwn"];

/// Resolves `link` (a percent-decoded relative path) from the document at `document`.
/// `is_repository` says whether a folder is a repository's root (has `.git`).
pub(crate) fn resolve(
    document: &Path,
    link: &str,
    fragment: Option<&str>,
    is_repository: &dyn Fn(&Path) -> bool,
) -> Opening {
    let path = resolve_path(document, link, is_repository);
    let markdown = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| MARKDOWN.contains(&extension.to_ascii_lowercase().as_str()));
    if markdown {
        Opening::Viewer {
            path,
            fragment: fragment.map(str::to_owned),
        }
    } else {
        Opening::System(path)
    }
}

/// The file a relative path names, from the document at `document`: from its folder, or for
/// `/path` from its repository's root (images resolve the same way).
pub(crate) fn resolve_path(
    document: &Path,
    link: &str,
    is_repository: &dyn Fn(&Path) -> bool,
) -> PathBuf {
    let folder = document.parent().unwrap_or_else(|| Path::new(""));
    match link.strip_prefix('/') {
        Some(from_root) => {
            let root = folder
                .ancestors()
                .find(|ancestor| is_repository(ancestor))
                .unwrap_or(folder);
            normalise(&root.join(from_root))
        }
        None => normalise(&folder.join(link)),
    }
}

/// `a/./b/../c` as `a/c`, without asking the file system (a `..` past the start of a relative
/// path is kept; past `/`, dropped).
fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                // `/..` is `/`, as the system has it.
                Some(Component::RootDir | Component::Prefix(_)) => {}
                Some(Component::ParentDir | Component::CurDir) | None => out.push(".."),
            },
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                out.push(component);
            }
        }
    }
    out
}

/// How the system is asked to open a file of a content type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Launch {
    /// With its default application: a type no default handler runs as a program.
    Default,
    /// Through the system's application chooser: a default handler might run it (Wine for
    /// `.exe`, Java for `.jar`, installers), so the person picks.
    Ask,
}

/// Content types opened with their default application without asking.
const OPENED_DIRECTLY: [&str; 4] = ["image/", "audio/", "video/", "font/"];
const OPENED_DIRECTLY_EXACT: [&str; 6] = [
    "application/pdf",
    "inode/directory",
    "text/plain",
    "text/markdown",
    "text/csv",
    "text/html",
];

pub(crate) fn launch(content_type: &str) -> Launch {
    let direct = OPENED_DIRECTLY
        .iter()
        .any(|prefix| content_type.starts_with(prefix))
        || OPENED_DIRECTLY_EXACT.contains(&content_type);
    if direct { Launch::Default } else { Launch::Ask }
}

/// Whether the system may be asked to open `path`: a folder, or a file that is not executable
/// and not a `.desktop` launcher (whose default handler might run it).
pub(crate) fn safe_to_open(path: &Path, metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    if metadata.is_dir() {
        return true;
    }
    let launcher = path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("desktop"));
    metadata.is_file() && metadata.permissions().mode() & 0o111 == 0 && !launcher
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_repository(_folder: &Path) -> bool {
        false
    }

    #[test]
    fn markdown_opens_in_the_viewer_at_its_fragment() {
        assert_eq!(
            resolve(
                Path::new("/r/docs/README.md"),
                "guide/Setup.MD",
                Some("install"),
                &no_repository
            ),
            Opening::Viewer {
                path: PathBuf::from("/r/docs/guide/Setup.MD"),
                fragment: Some("install".to_owned())
            }
        );
        assert_eq!(
            resolve(
                Path::new("/r/README.md"),
                "../other/x.markdown",
                None,
                &no_repository
            ),
            Opening::Viewer {
                path: PathBuf::from("/other/x.markdown"),
                fragment: None
            }
        );
    }

    #[test]
    fn other_files_go_to_the_system() {
        assert_eq!(
            resolve(Path::new("/r/README.md"), "./LICENSE", None, &no_repository),
            Opening::System(PathBuf::from("/r/LICENSE"))
        );
        assert_eq!(
            resolve(Path::new("/r/README.md"), "docs/", None, &no_repository),
            Opening::System(PathBuf::from("/r/docs"))
        );
    }

    #[test]
    fn a_leading_slash_starts_at_the_repository_root() {
        let is_repository = |folder: &Path| folder == Path::new("/r");
        assert_eq!(
            resolve(
                Path::new("/r/a/b/README.md"),
                "/CONTRIBUTING.md",
                None,
                &is_repository
            ),
            Opening::Viewer {
                path: PathBuf::from("/r/CONTRIBUTING.md"),
                fragment: None
            }
        );
        // No repository: the document's folder.
        assert_eq!(
            resolve(Path::new("/r/a/README.md"), "/x.png", None, &no_repository),
            Opening::System(PathBuf::from("/r/a/x.png"))
        );
    }

    #[test]
    fn normalising_keeps_parents_it_cannot_remove() {
        assert_eq!(normalise(Path::new("a/./b/../c")), PathBuf::from("a/c"));
        assert_eq!(normalise(Path::new("../../a")), PathBuf::from("../../a"));
        assert_eq!(normalise(Path::new("/../a")), PathBuf::from("/a"));
    }

    #[test]
    fn only_harmless_types_open_without_asking() {
        for direct in [
            "image/png",
            "video/mp4",
            "application/pdf",
            "inode/directory",
            "text/plain",
        ] {
            assert_eq!(launch(direct), Launch::Default, "{direct}");
        }
        for asked in [
            "application/x-ms-dos-executable",
            "application/x-java-archive",
            "application/x-msi",
            "application/x-desktop",
            "text/x-shellscript",
            "application/octet-stream",
        ] {
            assert_eq!(launch(asked), Launch::Ask, "{asked}");
        }
    }

    #[test]
    fn executables_and_launchers_are_not_opened() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("gbk-links-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let plain = dir.join("notes.txt");
        let script = dir.join("run.sh");
        let launcher = dir.join("app.desktop");
        for file in [&plain, &script, &launcher] {
            std::fs::write(file, "x").expect("write");
        }
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let safe = |path: &Path| safe_to_open(path, &std::fs::metadata(path).expect("metadata"));
        assert!(safe(&plain));
        assert!(safe(&dir));
        assert!(!safe(&script));
        assert!(!safe(&launcher));
        std::fs::remove_dir_all(&dir).expect("clean up");
    }
}
