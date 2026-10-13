//! Keyboard navigation and zoom (docs/plan.md §4.5, "As built in M6"): which key does what, where
//! a scroll lands, and the zoom steps. Pure, so it is unit-tested; the view applies the actions.

use gtk::gdk::{Key, ModifierType};

use crate::render::geometry::LINE_HEIGHT_EM;

/// Text sizes, as browsers step them: Ctrl+plus and Ctrl+minus move one step.
pub(crate) const ZOOMS: [f64; 13] = [
    0.5, 0.67, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0, 4.0,
];
/// A line's scroll: three lines of body text, in em.
const LINE_EM: f64 = 3.0 * LINE_HEIGHT_EM;
/// What a page scroll keeps in view of the page before: two lines, in em.
const PAGE_OVERLAP_EM: f64 = 2.0 * LINE_HEIGHT_EM;
/// A touchpad's Ctrl+scroll, in pixels, per zoom step.
const SURFACE_PER_STEP: f64 = 50.0;

/// What a key asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    Scroll(Scroll),
    /// Back to where the view was before the last jump to a link.
    Back,
    /// Forward again to where going back left.
    Forward,
    /// Open the file in the text editor.
    Edit,
    Zoom(Zoom),
    Reload,
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scroll {
    LineUp,
    LineDown,
    PageUp,
    PageDown,
    Top,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Zoom {
    In,
    Out,
    Reset,
}

/// The action for a key pressed with `modifiers`, if it has one.
pub(crate) fn action(key: Key, modifiers: ModifierType) -> Option<Action> {
    // Caps Lock, Num Lock and the like change nothing here; Super, Hyper and Meta are the
    // desktop's, so a key with them is not ours.
    let modifiers = modifiers
        & (ModifierType::CONTROL_MASK
            | ModifierType::SHIFT_MASK
            | ModifierType::ALT_MASK
            | ModifierType::SUPER_MASK
            | ModifierType::HYPER_MASK
            | ModifierType::META_MASK);
    let control = modifiers == ModifierType::CONTROL_MASK;
    // Ctrl+plus is Ctrl+Shift+equal on most layouts.
    let control_shifted = modifiers == ModifierType::CONTROL_MASK | ModifierType::SHIFT_MASK;
    let shift = modifiers == ModifierType::SHIFT_MASK;
    let alt = modifiers == ModifierType::ALT_MASK;
    let plain = modifiers.is_empty();
    let action = match key {
        Key::Up | Key::KP_Up if plain => Action::Scroll(Scroll::LineUp),
        Key::Down | Key::KP_Down if plain => Action::Scroll(Scroll::LineDown),
        Key::Page_Up | Key::KP_Page_Up if plain => Action::Scroll(Scroll::PageUp),
        Key::Page_Down | Key::KP_Page_Down | Key::space if plain => {
            Action::Scroll(Scroll::PageDown)
        }
        Key::space if shift => Action::Scroll(Scroll::PageUp),
        Key::Home | Key::KP_Home if plain || control => Action::Scroll(Scroll::Top),
        Key::End | Key::KP_End if plain || control => Action::Scroll(Scroll::Bottom),
        Key::Left if alt => Action::Back,
        Key::Right if alt => Action::Forward,
        Key::e | Key::E if control => Action::Edit,
        Key::BackSpace if plain => Action::Back,
        Key::plus | Key::equal | Key::KP_Add if control || control_shifted => {
            Action::Zoom(Zoom::In)
        }
        Key::minus | Key::KP_Subtract if control => Action::Zoom(Zoom::Out),
        // Ctrl+0 is Ctrl+Shift+0 where 0 needs Shift (AZERTY).
        Key::_0 | Key::KP_0 if control || control_shifted => Action::Zoom(Zoom::Reset),
        Key::r | Key::R if control => Action::Reload,
        Key::F5 if plain => Action::Reload,
        Key::w | Key::W | Key::q | Key::Q if control => Action::Close,
        _ => return None,
    };
    Some(action)
}

/// Where the view was before each jump to a link inside the document, and where going back
/// left, as a browser keeps them: back (Alt+Left, the back button) and forward (Alt+Right, the
/// forward button) step through them, and a new jump forgets the way forward. A place is the
/// view's position (`None`: the top of the document).
#[derive(Debug)]
pub(crate) struct History<T> {
    back: Vec<T>,
    forward: Vec<T>,
}

impl<T> Default for History<T> {
    fn default() -> Self {
        Self {
            back: Vec::new(),
            forward: Vec::new(),
        }
    }
}

impl<T> History<T> {
    /// A jump from `from`.
    pub(crate) fn jumped(&mut self, from: T) {
        self.back.push(from);
        self.forward.clear();
    }

    /// Back from `here`: where to go, if anywhere.
    pub(crate) fn back(&mut self, here: T) -> Option<T> {
        let to = self.back.pop()?;
        self.forward.push(here);
        Some(to)
    }

    /// Forward from `here`: where to go, if anywhere.
    pub(crate) fn forward(&mut self, here: T) -> Option<T> {
        let to = self.forward.pop()?;
        self.back.push(here);
        Some(to)
    }
}

/// A key written as GDK names it, after any of `ctrl+`, `shift+` and `alt+` (`ctrl+plus`,
/// `Page_Down`), for `--press`.
pub(crate) fn parse_key(text: &str) -> Option<(Key, ModifierType)> {
    let mut modifiers = ModifierType::empty();
    let mut rest = text;
    loop {
        let lower = rest.to_ascii_lowercase();
        let (modifier, length) = if lower.starts_with("ctrl+") {
            (ModifierType::CONTROL_MASK, 5)
        } else if lower.starts_with("shift+") {
            (ModifierType::SHIFT_MASK, 6)
        } else if lower.starts_with("alt+") {
            (ModifierType::ALT_MASK, 4)
        } else {
            break;
        };
        modifiers |= modifier;
        rest = rest.get(length..)?;
    }
    Some((Key::from_name(rest)?, modifiers))
}

/// Where a scroll from `value` lands, for a view `page` high over a document `upper` high, with
/// body text `em` pixels high.
pub(crate) fn scroll_target(scroll: Scroll, value: f64, page: f64, upper: f64, em: f64) -> f64 {
    let line = LINE_EM * em;
    let page_step = (page - PAGE_OVERLAP_EM * em).max(line);
    let target = match scroll {
        Scroll::LineUp => value - line,
        Scroll::LineDown => value + line,
        Scroll::PageUp => value - page_step,
        Scroll::PageDown => value + page_step,
        Scroll::Top => 0.0,
        Scroll::Bottom => upper,
    };
    target.clamp(0.0, (upper - page).max(0.0))
}

/// The zoom one step from `current`: the next step up or down `ZOOMS` from wherever `current`
/// is (a `--zoom` may fall between steps), or 1 to reset; `None` if that is no change (at an
/// end, or already 1).
pub(crate) fn zoomed(current: f64, zoom: Zoom) -> Option<f64> {
    let next = match zoom {
        Zoom::Reset => Some(1.0),
        Zoom::In => ZOOMS.iter().copied().find(|&step| step > current + 1e-9),
        Zoom::Out => ZOOMS
            .iter()
            .rev()
            .copied()
            .find(|&step| step < current - 1e-9),
    }?;
    ((next - current).abs() > 1e-9).then_some(next)
}

/// Ctrl+wheel steps: scroll deltas added up, one step per wheel notch or `SURFACE_PER_STEP`
/// pixels of touchpad (which sends many small deltas: a step for each zoomed straight to the
/// end, M6 review).
#[derive(Debug, Default)]
pub(crate) struct WheelZoom {
    pending: f64,
}

impl WheelZoom {
    /// Adds a scroll of `dy` (pixels if `pixels`, else wheel notches); returns the step it
    /// completes, if any. Turning back starts over.
    pub(crate) fn scrolled(&mut self, dy: f64, pixels: bool) -> Option<Zoom> {
        let notches = if pixels { dy / SURFACE_PER_STEP } else { dy };
        if notches * self.pending < 0.0 {
            self.pending = 0.0;
        }
        self.pending += notches;
        if self.pending <= -1.0 {
            self.pending += 1.0;
            Some(Zoom::In)
        } else if self.pending >= 1.0 {
            self.pending -= 1.0;
            Some(Zoom::Out)
        } else {
            None
        }
    }
}

/// A `--zoom` percentage as a factor, if it is within the steps.
pub(crate) fn zoom_from_percent(percent: u32) -> Option<f64> {
    let factor = f64::from(percent) / 100.0;
    let (smallest, largest) = (ZOOMS.first()?, ZOOMS.last()?);
    (*smallest..=*largest).contains(&factor).then_some(factor)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: ModifierType = ModifierType::empty();
    const CTRL: ModifierType = ModifierType::CONTROL_MASK;
    const SHIFT: ModifierType = ModifierType::SHIFT_MASK;
    const ALT: ModifierType = ModifierType::ALT_MASK;

    #[test]
    fn keys_scroll_zoom_reload_and_close() {
        let cases = [
            (Key::Down, NONE, Some(Action::Scroll(Scroll::LineDown))),
            (Key::KP_Up, NONE, Some(Action::Scroll(Scroll::LineUp))),
            (Key::Page_Down, NONE, Some(Action::Scroll(Scroll::PageDown))),
            (Key::KP_Page_Up, NONE, Some(Action::Scroll(Scroll::PageUp))),
            (Key::space, NONE, Some(Action::Scroll(Scroll::PageDown))),
            (Key::space, SHIFT, Some(Action::Scroll(Scroll::PageUp))),
            (Key::Home, NONE, Some(Action::Scroll(Scroll::Top))),
            (Key::Home, CTRL, Some(Action::Scroll(Scroll::Top))),
            (Key::KP_End, NONE, Some(Action::Scroll(Scroll::Bottom))),
            (Key::End, CTRL, Some(Action::Scroll(Scroll::Bottom))),
            (Key::Left, ALT, Some(Action::Back)),
            (Key::Right, ALT, Some(Action::Forward)),
            (Key::Right, NONE, None),
            (Key::Right, CTRL, None),
            (Key::e, CTRL, Some(Action::Edit)),
            (Key::E, CTRL, Some(Action::Edit)),
            (Key::e, NONE, None),
            (Key::e, ALT, None),
            (Key::BackSpace, NONE, Some(Action::Back)),
            (Key::plus, CTRL, Some(Action::Zoom(Zoom::In))),
            (Key::plus, CTRL | SHIFT, Some(Action::Zoom(Zoom::In))),
            (Key::equal, CTRL, Some(Action::Zoom(Zoom::In))),
            (Key::KP_Add, CTRL, Some(Action::Zoom(Zoom::In))),
            (Key::minus, CTRL, Some(Action::Zoom(Zoom::Out))),
            (Key::KP_Subtract, CTRL, Some(Action::Zoom(Zoom::Out))),
            (Key::_0, CTRL, Some(Action::Zoom(Zoom::Reset))),
            (Key::_0, CTRL | SHIFT, Some(Action::Zoom(Zoom::Reset))),
            (Key::Home, ModifierType::SUPER_MASK, None),
            (Key::r, CTRL | ModifierType::META_MASK, None),
            (Key::r, CTRL, Some(Action::Reload)),
            (Key::F5, NONE, Some(Action::Reload)),
            (Key::w, CTRL, Some(Action::Close)),
            (Key::Q, CTRL, Some(Action::Close)),
            // Caps Lock does not get in the way.
            (Key::R, CTRL | ModifierType::LOCK_MASK, Some(Action::Reload)),
            // Wrong or extra modifiers: nothing.
            (Key::Left, NONE, None),
            (Key::Down, CTRL, None),
            (Key::Up, CTRL, None),
            (Key::Page_Up, SHIFT, None),
            (Key::Down, SHIFT, None),
            (Key::Page_Down, ALT, None),
            (Key::space, CTRL, None),
            (Key::Home, SHIFT, None),
            (Key::End, ALT, None),
            (Key::BackSpace, CTRL, None),
            (Key::plus, NONE, None),
            (Key::minus, CTRL | SHIFT, None),
            (Key::_0, NONE, None),
            (Key::r, NONE, None),
            (Key::F5, CTRL, None),
            (Key::w, NONE, None),
            (Key::q, ALT, None),
            (Key::a, NONE, None),
        ];
        for (key, modifiers, expected) in cases {
            assert_eq!(action(key, modifiers), expected, "{key:?} {modifiers:?}");
        }
    }

    #[test]
    fn history_goes_back_and_forward_and_a_jump_forgets_forward() {
        let mut history = History::default();
        assert_eq!(history.back(0), None);
        // At 0, jump to 10; at 10, jump to 20.
        history.jumped(0);
        history.jumped(10);
        assert_eq!(history.back(20), Some(10));
        assert_eq!(history.back(10), Some(0));
        assert_eq!(history.back(0), None);
        assert_eq!(history.forward(0), Some(10));
        assert_eq!(history.forward(10), Some(20));
        assert_eq!(history.forward(20), None);
        // Back once, then a new jump: nothing forward.
        assert_eq!(history.back(20), Some(10));
        history.jumped(10);
        assert_eq!(history.forward(30), None);
        assert_eq!(history.back(30), Some(10));
    }

    #[test]
    fn keys_are_read_as_gdk_names_them() {
        assert_eq!(parse_key("Page_Down"), Some((Key::Page_Down, NONE)));
        assert_eq!(parse_key("ctrl+plus"), Some((Key::plus, CTRL)));
        assert_eq!(
            parse_key("Ctrl+Shift+plus"),
            Some((Key::plus, CTRL | SHIFT))
        );
        assert_eq!(parse_key("alt+Left"), Some((Key::Left, ALT)));
        assert_eq!(parse_key("ctrl+"), None);
        assert_eq!(parse_key("nonesuch"), None);
    }

    #[test]
    fn scrolls_land_within_the_document() {
        let (page, upper, em) = (600.0, 5000.0, 10.0);
        let at = |scroll, value| scroll_target(scroll, value, page, upper, em);
        assert!((at(Scroll::LineDown, 100.0) - 142.0).abs() < 1e-9);
        assert!((at(Scroll::LineUp, 100.0) - 58.0).abs() < 1e-9);
        // A page less two lines of overlap.
        assert!((at(Scroll::PageDown, 100.0) - 672.0).abs() < 1e-9);
        assert!((at(Scroll::PageUp, 1000.0) - 428.0).abs() < 1e-9);
        assert!(at(Scroll::Top, 1000.0).abs() < 1e-9);
        // The bottom is where the last page shows.
        assert!((at(Scroll::Bottom, 0.0) - 4400.0).abs() < 1e-9);
        assert!((at(Scroll::PageDown, 4300.0) - 4400.0).abs() < 1e-9);
        assert!(at(Scroll::LineUp, 10.0).abs() < 1e-9);
        // A document shorter than the page does not scroll.
        assert!(scroll_target(Scroll::PageDown, 0.0, 600.0, 300.0, em).abs() < 1e-9);
        // A page smaller than its overlap still moves a line.
        assert!((scroll_target(Scroll::PageDown, 0.0, 20.0, upper, em) - 42.0).abs() < 1e-9);
    }

    #[test]
    fn zoom_steps_up_down_and_back() {
        let step = |current, zoom| zoomed(current, zoom).expect("a change");
        assert!((step(1.0, Zoom::In) - 1.1).abs() < 1e-9);
        assert!((step(1.0, Zoom::Out) - 0.9).abs() < 1e-9);
        assert!((step(2.5, Zoom::Reset) - 1.0).abs() < 1e-9);
        // Between steps (a --zoom of 120%), to the neighbouring steps.
        assert!((step(1.2, Zoom::In) - 1.25).abs() < 1e-9);
        assert!((step(1.2, Zoom::Out) - 1.1).abs() < 1e-9);
        // At the ends, or reset at 1: no change.
        assert_eq!(zoomed(4.0, Zoom::In), None);
        assert_eq!(zoomed(0.5, Zoom::Out), None);
        assert_eq!(zoomed(1.0, Zoom::Reset), None);
        assert_eq!(zoom_from_percent(150), Some(1.5));
        assert_eq!(zoom_from_percent(50), Some(0.5));
        assert_eq!(zoom_from_percent(400), Some(4.0));
        assert_eq!(zoom_from_percent(49), None);
        assert_eq!(zoom_from_percent(401), None);
    }

    #[test]
    fn the_wheel_zooms_a_step_a_notch_and_a_touchpad_by_distance() {
        let mut wheel = WheelZoom::default();
        assert_eq!(wheel.scrolled(-1.0, false), Some(Zoom::In));
        assert_eq!(wheel.scrolled(1.0, false), Some(Zoom::Out));
        // A smooth wheel's fractions: one step per whole notch.
        assert_eq!(wheel.scrolled(0.4, false), None);
        assert_eq!(wheel.scrolled(0.4, false), None);
        assert_eq!(wheel.scrolled(0.4, false), Some(Zoom::Out));
        // A touchpad: 12 deltas of 5 px is 60 px, one step.
        let mut touchpad = WheelZoom::default();
        let steps: Vec<Zoom> = (0..12)
            .filter_map(|_| touchpad.scrolled(-5.0, true))
            .collect();
        assert_eq!(steps, vec![Zoom::In]);
        // Turning back starts over: 40 px one way, then 40 the other, is no step.
        let mut turned = WheelZoom::default();
        assert_eq!(turned.scrolled(-40.0, true), None);
        assert_eq!(turned.scrolled(40.0, true), None);
        assert_eq!(turned.scrolled(15.0, true), Some(Zoom::Out));
    }
}
