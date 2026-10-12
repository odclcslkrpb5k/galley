//! Choosing GTK's renderer (docs/plan.md §4.4).
//!
//! Cairo is the default: with GL disabled, GDK does not load Mesa, and the first frame comes
//! 70-90 ms sooner (docs/perf-baseline.md, M2); scrolling is about even. GTK reads its renderer
//! from the environment when it starts, so the choice is made by setting `GSK_RENDERER` (and,
//! for Cairo, adding `gl` and `vulkan` to `GDK_DISABLE`, without which GDK still prepares GL).
//! This module decides what to set; `main` sets it, in the one `unsafe` block of the program.

/// The value of `--renderer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Renderer {
    Cairo,
    Gl,
}

/// A change to make to the environment before GTK starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EnvChange {
    Set(&'static str, String),
    Remove(&'static str),
}

const GSK_RENDERER: &str = "GSK_RENDERER";
const GDK_DISABLE: &str = "GDK_DISABLE";

/// The environment changes for `--renderer` (`flag`), given the current `GSK_RENDERER` and
/// `GDK_DISABLE`. The flag overrides `GSK_RENDERER`; without the flag a renderer the person set
/// in `GSK_RENDERER` is left alone; with neither, Cairo.
pub(crate) fn env_changes(
    flag: Option<Renderer>,
    gsk_renderer: Option<&str>,
    gdk_disable: Option<&str>,
) -> Vec<EnvChange> {
    let renderer = match (flag, gsk_renderer.filter(|value| !value.is_empty())) {
        (Some(renderer), _) => renderer,
        (None, None | Some("cairo")) => Renderer::Cairo,
        (None, Some(_their_own)) => return Vec::new(),
    };
    let disabled: Vec<&str> = gdk_disable
        .unwrap_or_default()
        // GDK's debug-key separators.
        .split([',', ':', ';', ' ', '\t'])
        .filter(|key| !key.is_empty())
        .collect();
    match renderer {
        Renderer::Cairo => {
            let mut keys = disabled;
            for key in ["gl", "vulkan"] {
                if !keys.contains(&key) {
                    keys.push(key);
                }
            }
            vec![
                EnvChange::Set(GSK_RENDERER, "cairo".to_owned()),
                EnvChange::Set(GDK_DISABLE, keys.join(",")),
            ]
        }
        Renderer::Gl => {
            let mut changes = vec![EnvChange::Set(GSK_RENDERER, "gl".to_owned())];
            if disabled.contains(&"gl") {
                let keys: Vec<&str> = disabled.into_iter().filter(|&key| key != "gl").collect();
                changes.push(if keys.is_empty() {
                    EnvChange::Remove(GDK_DISABLE)
                } else {
                    EnvChange::Set(GDK_DISABLE, keys.join(","))
                });
            }
            changes
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cairo(disable: &str) -> Vec<EnvChange> {
        vec![
            EnvChange::Set(GSK_RENDERER, "cairo".to_owned()),
            EnvChange::Set(GDK_DISABLE, disable.to_owned()),
        ]
    }

    #[test]
    fn cairo_is_the_default() {
        assert_eq!(env_changes(None, None, None), cairo("gl,vulkan"));
        assert_eq!(env_changes(None, Some(""), None), cairo("gl,vulkan"));
        assert_eq!(env_changes(None, Some("cairo"), None), cairo("gl,vulkan"));
    }

    #[test]
    fn the_flag_overrides_gsk_renderer() {
        assert_eq!(
            env_changes(Some(Renderer::Cairo), Some("gl"), None),
            cairo("gl,vulkan")
        );
        assert_eq!(
            env_changes(Some(Renderer::Gl), Some("cairo"), None),
            vec![EnvChange::Set(GSK_RENDERER, "gl".to_owned())]
        );
    }

    #[test]
    fn without_the_flag_a_renderer_set_in_the_environment_is_kept() {
        assert_eq!(env_changes(None, Some("gl"), None), Vec::new());
        assert_eq!(
            env_changes(None, Some("vulkan"), Some("dmabuf")),
            Vec::new()
        );
    }

    #[test]
    fn other_disabled_features_are_kept() {
        assert_eq!(
            env_changes(None, None, Some("dmabuf,vulkan")),
            cairo("dmabuf,vulkan,gl")
        );
        assert_eq!(
            env_changes(None, None, Some("gl:vulkan")),
            cairo("gl,vulkan")
        );
        assert_eq!(
            env_changes(Some(Renderer::Gl), None, Some("gl, dmabuf")),
            vec![
                EnvChange::Set(GSK_RENDERER, "gl".to_owned()),
                EnvChange::Set(GDK_DISABLE, "dmabuf".to_owned())
            ]
        );
        assert_eq!(
            env_changes(Some(Renderer::Gl), None, Some("gl")),
            vec![
                EnvChange::Set(GSK_RENDERER, "gl".to_owned()),
                EnvChange::Remove(GDK_DISABLE)
            ]
        );
        assert_eq!(
            env_changes(Some(Renderer::Gl), None, Some("vulkan")),
            vec![EnvChange::Set(GSK_RENDERER, "gl".to_owned())]
        );
    }
}
