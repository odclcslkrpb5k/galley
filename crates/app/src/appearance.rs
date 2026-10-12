//! Light or dark (docs/plan.md §4.5, "As built in M6"). GTK's default theme follows its
//! interface colour scheme, which GTK reads from the desktop's settings portal; the view's
//! palette follows the theme's text colour, so the document, its code colours included, follows
//! too. Pure, so it is unit-tested; `main` applies the result.

use gtk::InterfaceColorScheme;

/// The value of `--theme`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Theme {
    /// As the desktop says.
    #[default]
    System,
    Light,
    Dark,
}

/// The colour scheme to set, if any, for `theme` when GTK has `current` and the desktop's GTK
/// theme is called `theme_name`. With `System`, a preference the portal gave is kept; with none
/// (Xfce's portal often has none), a GTK theme named `*-dark` (Greybird-dark, Adwaita-dark)
/// means dark.
pub(crate) fn color_scheme(
    theme: Theme,
    theme_name: Option<&str>,
    current: InterfaceColorScheme,
) -> Option<InterfaceColorScheme> {
    match theme {
        Theme::Light => Some(InterfaceColorScheme::Light),
        Theme::Dark => Some(InterfaceColorScheme::Dark),
        Theme::System => {
            let stated = matches!(
                current,
                InterfaceColorScheme::Light | InterfaceColorScheme::Dark
            );
            let dark_name =
                theme_name.is_some_and(|name| name.to_lowercase().trim_end().ends_with("-dark"));
            (!stated && dark_name).then_some(InterfaceColorScheme::Dark)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chosen_theme_wins_and_the_system_is_followed_otherwise() {
        use InterfaceColorScheme::{Dark, Default, Light, Unsupported};
        let cases = [
            (Theme::Light, Some("Adwaita-dark"), Dark, Some(Light)),
            (Theme::Dark, Some("Classic-1.0"), Light, Some(Dark)),
            (Theme::Dark, None, Unsupported, Some(Dark)),
            // The portal said: kept, whatever the theme is called.
            (Theme::System, Some("Greybird-dark"), Light, None),
            (Theme::System, Some("Classic-1.0"), Dark, None),
            // No word from the portal: the theme's name decides.
            (Theme::System, Some("Greybird-dark"), Default, Some(Dark)),
            (Theme::System, Some("Adwaita-Dark"), Unsupported, Some(Dark)),
            (Theme::System, Some("Classic-1.0"), Default, None),
            (Theme::System, Some("darkness"), Default, None),
            (Theme::System, None, Unsupported, None),
        ];
        for (theme, name, current, expected) in cases {
            assert_eq!(
                color_scheme(theme, name, current),
                expected,
                "{theme:?} {name:?}"
            );
        }
    }
}
