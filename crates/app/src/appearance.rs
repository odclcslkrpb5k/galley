//! Light or dark (docs/plan.md §4.5, "As built in M6"). GTK's default theme follows its
//! interface colour scheme, which GTK reads from the desktop's settings portal; the view's
//! palette follows the theme's text colour, so the document, its code colours included, follows
//! too. Pure, so it is unit-tested; `main` applies the results.
//!
//! A chosen theme is set through `GTK_THEME` before GTK starts: setting the colour scheme after
//! it has started makes GTK load and parse its theme again, 15-33 ms before the first frame
//! (M6 review); through the environment it costs nothing.

use gtk::InterfaceColorScheme;

use crate::renderer::EnvChange;

const GTK_THEME: &str = "GTK_THEME";

/// The value of `--theme`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Theme {
    /// As the desktop says.
    #[default]
    System,
    Light,
    Dark,
}

/// The environment change for `theme`: GTK's own theme in the chosen variant, overriding any
/// `GTK_THEME` (the flag is the more specific choice, as `--renderer` over `GSK_RENDERER`).
pub(crate) fn env_change(theme: Theme) -> Option<EnvChange> {
    match theme {
        Theme::System => None,
        Theme::Light => Some(EnvChange::Set(GTK_THEME, "Default".to_owned())),
        Theme::Dark => Some(EnvChange::Set(GTK_THEME, "Default:dark".to_owned())),
    }
}

/// With `--theme=system`, once GTK has started: the colour scheme to set, if any, when GTK has
/// `current` and the desktop's GTK theme is called `theme_name`. A preference the portal gave
/// is kept; with none (Xfce's portal often has none), a GTK theme named `*-dark`
/// (Greybird-dark, Adwaita-dark) means dark. Only that case pays GTK's second theme load.
pub(crate) fn system_scheme(
    theme_name: Option<&str>,
    current: InterfaceColorScheme,
) -> Option<InterfaceColorScheme> {
    let stated = matches!(
        current,
        InterfaceColorScheme::Light | InterfaceColorScheme::Dark
    );
    let dark_name =
        theme_name.is_some_and(|name| name.to_lowercase().trim_end().ends_with("-dark"));
    (!stated && dark_name).then_some(InterfaceColorScheme::Dark)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chosen_theme_is_set_before_gtk_starts() {
        assert_eq!(env_change(Theme::System), None);
        assert_eq!(
            env_change(Theme::Light),
            Some(EnvChange::Set(GTK_THEME, "Default".to_owned()))
        );
        assert_eq!(
            env_change(Theme::Dark),
            Some(EnvChange::Set(GTK_THEME, "Default:dark".to_owned()))
        );
    }

    #[test]
    fn the_system_is_followed_and_a_dark_theme_name_read_when_it_says_nothing() {
        use InterfaceColorScheme::{Dark, Default, Light, Unsupported};
        let cases = [
            // The portal said: kept, whatever the theme is called.
            (Some("Greybird-dark"), Light, None),
            (Some("Classic-1.0"), Dark, None),
            // No word from the portal: the theme's name decides.
            (Some("Greybird-dark"), Default, Some(Dark)),
            (Some("Adwaita-Dark"), Unsupported, Some(Dark)),
            (Some("Classic-1.0"), Default, None),
            (Some("darkness"), Default, None),
            (None, Unsupported, None),
        ];
        for (name, current, expected) in cases {
            assert_eq!(system_scheme(name, current), expected, "{name:?}");
        }
    }
}
