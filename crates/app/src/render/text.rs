//! Turning a `LeafPaint` into a Pango layout (docs/plan.md §4.5).
//!
//! Used on the main thread by the widget and on the height worker, through the same
//! `Typesetter::measure`, so both measure the same height. Colours do not change a layout's
//! size; the worker uses a default palette.

use gtk::pango;
use gtk::prelude::*;

use super::geometry::Metrics;
use super::paint::{AttrKind, FontRole, LeafPaint, TextAttr};
use crate::units;
use gbk_markdown_core::model::{AlertKind, Align, HeadingLevel};

/// Body text is sans-serif and code monospace, at the desktop font's size: the desktop font may
/// itself be monospace (this host's is Source Code Pro), which suits a terminal, not a document.
const BODY_FAMILY: &str = "sans-serif";
const CODE_FAMILY: &str = "monospace";

/// Everything that decides how text is laid out, copied from the widget's Pango context so the
/// height worker's own context matches it. Plain data, so it can cross to the worker.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TextSetup {
    /// Body font size in points.
    pub(crate) size_pt: f64,
    /// Pixels per inch.
    pub(crate) resolution: f64,
    pub(crate) antialias: gtk::cairo::Antialias,
    pub(crate) hint_style: gtk::cairo::HintStyle,
    pub(crate) hint_metrics: gtk::cairo::HintMetrics,
    pub(crate) round_glyph_positions: bool,
}

impl TextSetup {
    /// The setup a widget lays text out with.
    ///
    /// Resolution, font size and glyph rounding are read from the widget's Pango context. Its
    /// cairo font options cannot be read back (pangocairo has no binding for the getter), so
    /// they are derived from GTK's settings as GTK derives them: in the default "automatic"
    /// font-rendering mode grey antialiasing, slight hinting and no metric hinting; in "manual"
    /// mode the `gtk-xft-*` settings. The `--check-heights` smoke run proves the result matches.
    pub(crate) fn of(widget: &impl IsA<gtk::Widget>) -> Self {
        let context = widget.pango_context();
        let size = context
            .font_description()
            .map_or(10.0, |font| units::from_pango_units(font.size()));
        let resolution = pangocairo::functions::context_get_resolution(&context);
        let (antialias, hint_style, hint_metrics) = font_options(&widget.settings());
        Self {
            size_pt: if size > 0.0 { size } else { 10.0 },
            resolution: if resolution > 0.0 { resolution } else { 96.0 },
            antialias,
            hint_style,
            hint_metrics,
            round_glyph_positions: context.is_round_glyph_positions(),
        }
    }

    /// Applies this setup to a context the worker made.
    pub(crate) fn apply(self, context: &pango::Context) {
        pangocairo::functions::context_set_resolution(context, self.resolution);
        if let Ok(mut options) = gtk::cairo::FontOptions::new() {
            options.set_antialias(self.antialias);
            options.set_hint_style(self.hint_style);
            options.set_hint_metrics(self.hint_metrics);
            pangocairo::functions::context_set_font_options(context, Some(&options));
        }
        context.set_round_glyph_positions(self.round_glyph_positions);
    }

    pub(crate) fn metrics(self) -> Metrics {
        Metrics {
            em_px: self.size_pt * self.resolution / 72.0,
        }
    }

    fn font(self, role: FontRole) -> pango::FontDescription {
        let mut font = pango::FontDescription::new();
        font.set_family(match role {
            FontRole::Body | FontRole::Heading(_) => BODY_FAMILY,
            FontRole::Code => CODE_FAMILY,
        });
        font.set_size(units::to_pango_units(self.size_pt * role.scale()));
        font
    }
}

/// Colours for text attributes, from the widget's foreground and a light or dark scheme.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Palette {
    pub(crate) foreground: gtk::gdk::RGBA,
    pub(crate) link: gtk::gdk::RGBA,
    pub(crate) dark: bool,
}

impl Default for Palette {
    fn default() -> Self {
        Self::for_foreground(gtk::gdk::RGBA::BLACK)
    }
}

impl Palette {
    pub(crate) fn for_foreground(foreground: gtk::gdk::RGBA) -> Self {
        let dark = foreground.red() + foreground.green() + foreground.blue() > 1.5;
        Self {
            foreground,
            link: if dark {
                rgba(0x44, 0x93, 0xf8)
            } else {
                rgba(0x09, 0x69, 0xda)
            },
            dark,
        }
    }

    /// The foreground at `alpha` opacity: rules, bars, code backgrounds.
    pub(crate) fn faint(self, alpha: f32) -> gtk::gdk::RGBA {
        self.foreground.with_alpha(alpha)
    }

    /// GitHub's alert colours.
    pub(crate) fn alert(self, kind: AlertKind) -> gtk::gdk::RGBA {
        let (light, dark) = match kind {
            AlertKind::Note => ((0x09, 0x69, 0xda), (0x44, 0x93, 0xf8)),
            AlertKind::Tip => ((0x1a, 0x7f, 0x37), (0x3f, 0xb9, 0x50)),
            AlertKind::Important => ((0x82, 0x50, 0xdf), (0xab, 0x7d, 0xf8)),
            AlertKind::Warning => ((0x9a, 0x67, 0x00), (0xd2, 0x99, 0x22)),
            AlertKind::Caution => ((0xcf, 0x22, 0x2e), (0xf8, 0x51, 0x49)),
        };
        let (r, g, b) = if self.dark { dark } else { light };
        rgba(r, g, b)
    }
}

/// Cairo font options as GTK sets them for its widgets (see `TextSetup::of`).
fn font_options(
    settings: &gtk::Settings,
) -> (
    gtk::cairo::Antialias,
    gtk::cairo::HintStyle,
    gtk::cairo::HintMetrics,
) {
    use gtk::cairo::{Antialias, HintMetrics, HintStyle};
    if settings.gtk_font_rendering() == gtk::FontRendering::Manual {
        let antialias = match settings.gtk_xft_antialias() {
            0 => Antialias::None,
            _ => Antialias::Gray,
        };
        let hint_style = match settings.gtk_xft_hintstyle().as_deref() {
            Some("hintnone") => HintStyle::None,
            Some("hintslight") => HintStyle::Slight,
            Some("hintmedium") => HintStyle::Medium,
            Some("hintfull") => HintStyle::Full,
            _ => HintStyle::Default,
        };
        let hint_metrics = if settings.is_gtk_hint_font_metrics() {
            HintMetrics::On
        } else {
            HintMetrics::Off
        };
        (antialias, hint_style, hint_metrics)
    } else {
        (Antialias::Gray, HintStyle::Slight, HintMetrics::Off)
    }
}

fn rgba(red: u8, green: u8, blue: u8) -> gtk::gdk::RGBA {
    gtk::gdk::RGBA::new(
        f32::from(red) / 255.0,
        f32::from(green) / 255.0,
        f32::from(blue) / 255.0,
        1.0,
    )
}

/// Lays out text the one way the widget and the worker both use.
#[derive(Debug, Clone)]
pub(crate) struct Typesetter {
    pub(crate) context: pango::Context,
    pub(crate) setup: TextSetup,
    pub(crate) palette: Palette,
}

impl Typesetter {
    /// Lays out a leaf's text for a view `view_text_width` wide; returns the layout and its
    /// height. `counted` is told the bytes laid out (the widget's probe counts them).
    pub(crate) fn measure(
        &self,
        paint: &LeafPaint,
        view_text_width: i64,
        counted: &mut dyn FnMut(usize),
    ) -> (pango::Layout, i64) {
        let width = self.setup.metrics().text_width(paint, view_text_width);
        let layout = layout(
            &self.context,
            self.setup,
            self.palette,
            paint,
            width,
            counted,
        );
        let height = i64::from(layout.pixel_size().1);
        (layout, height)
    }

    /// A marker (list bullet, number, summary triangle) in the body font.
    pub(crate) fn marker(&self, text: &str, counted: &mut dyn FnMut(usize)) -> pango::Layout {
        let layout = new_layout(&self.context, text, counted);
        layout.set_font_description(Some(&self.setup.font(FontRole::Body)));
        layout
    }
}

/// Loads the fonts documents use (body, bold, italic, headings, code, markers) by laying out a
/// sample in each, until `done` (the loader has finished). A font's first use costs 3-15 ms of
/// fontconfig matching (M2 review), which otherwise falls into the first screen's layout.
pub(crate) fn warm_up(
    widget: &impl IsA<gtk::Widget>,
    done: &dyn Fn() -> bool,
    counted: &mut dyn FnMut(usize),
) {
    let typesetter = Typesetter {
        context: widget.pango_context(),
        setup: TextSetup::of(widget),
        palette: Palette::default(),
    };
    let text = "Ag 1. •◦▪☑☐▾";
    let styled = |kind| {
        vec![TextAttr {
            start: 0,
            end: text.len(),
            kind,
        }]
    };
    let samples = [
        LeafPaint::plain(text, FontRole::Body, Vec::new()),
        LeafPaint::plain(text, FontRole::Body, styled(AttrKind::Bold)),
        LeafPaint::plain(text, FontRole::Code, Vec::new()),
        LeafPaint::plain(text, FontRole::Body, styled(AttrKind::Italic)),
        LeafPaint::plain(
            text,
            FontRole::Heading(HeadingLevel::H1),
            styled(AttrKind::Bold),
        ),
        LeafPaint::plain(
            text,
            FontRole::Heading(HeadingLevel::H2),
            styled(AttrKind::Bold),
        ),
        LeafPaint::plain(
            text,
            FontRole::Heading(HeadingLevel::H3),
            styled(AttrKind::Bold),
        ),
    ];
    for sample in &samples {
        if done() {
            return;
        }
        drop(typesetter.measure(sample, 600, counted));
    }
}

/// Lays out a leaf's text at `width` logical pixels.
fn layout(
    context: &pango::Context,
    setup: TextSetup,
    palette: Palette,
    paint: &LeafPaint,
    width: i64,
    counted: &mut dyn FnMut(usize),
) -> pango::Layout {
    let layout = new_layout(context, &paint.text, counted);
    layout.set_font_description(Some(&setup.font(paint.font)));
    layout.set_width(units::to_pango_units(units::px_to_f64(width)));
    layout.set_wrap(pango::WrapMode::WordChar);
    layout.set_alignment(match paint.align {
        Align::Start => pango::Alignment::Left,
        Align::Center => pango::Alignment::Center,
        Align::End => pango::Alignment::Right,
    });
    let attrs = pango::AttrList::new();
    let em = units::to_pango_units(setup.metrics().em_px);
    for attr in &paint.attrs {
        let (start, end) = (units::text_index(attr.start), units::text_index(attr.end));
        let add = |mut attribute: pango::Attribute| {
            attribute.set_start_index(start);
            attribute.set_end_index(end);
            attrs.insert(attribute);
        };
        match attr.kind {
            AttrKind::Bold => add(pango::AttrInt::new_weight(pango::Weight::Bold).into()),
            AttrKind::Italic => add(pango::AttrInt::new_style(pango::Style::Italic).into()),
            AttrKind::Strikethrough => add(pango::AttrInt::new_strikethrough(true).into()),
            AttrKind::Underline => {
                add(pango::AttrInt::new_underline(pango::Underline::Single).into());
            }
            AttrKind::Code => {
                add(pango::AttrString::new_family(CODE_FAMILY).into());
                add(pango::AttrFloat::new_scale(FontRole::Code.scale()).into());
                for attribute in background(palette.faint(0.08)) {
                    add(attribute);
                }
            }
            AttrKind::Link => {
                for attribute in foreground(palette.link) {
                    add(attribute);
                }
                add(pango::AttrInt::new_underline(pango::Underline::Single).into());
            }
            AttrKind::Subscript | AttrKind::Superscript => {
                let rise = if attr.kind == AttrKind::Superscript {
                    em / 3
                } else {
                    -em / 5
                };
                add(pango::AttrInt::new_rise(rise).into());
                add(pango::AttrFloat::new_scale(0.75).into());
            }
            AttrKind::Highlight => {
                for attribute in background(gtk::gdk::RGBA::new(1.0, 0.85, 0.2, 0.5)) {
                    add(attribute);
                }
            }
            AttrKind::Muted => {
                for attribute in foreground(palette.faint(0.6)) {
                    add(attribute);
                }
            }
            AttrKind::AlertTitle(kind) => {
                for attribute in foreground(palette.alert(kind)) {
                    add(attribute);
                }
            }
        }
    }
    layout.set_attributes(Some(&attrs));
    layout
}

/// A text colour, as Pango has it: the colour and, if not opaque, its alpha.
fn foreground(color: gtk::gdk::RGBA) -> Vec<pango::Attribute> {
    let (red, green, blue, alpha) = units::rgba_u16(color);
    let mut attributes = vec![pango::AttrColor::new_foreground(red, green, blue).into()];
    if alpha < u16::MAX {
        attributes.push(pango::AttrInt::new_foreground_alpha(alpha).into());
    }
    attributes
}

/// A background colour, as Pango has it.
fn background(color: gtk::gdk::RGBA) -> Vec<pango::Attribute> {
    let (red, green, blue, alpha) = units::rgba_u16(color);
    let mut attributes = vec![pango::AttrColor::new_background(red, green, blue).into()];
    if alpha < u16::MAX {
        attributes.push(pango::AttrInt::new_background_alpha(alpha).into());
    }
    attributes
}

/// The one way the viewer creates a Pango layout, so all layout work is counted.
fn new_layout(
    context: &pango::Context,
    text: &str,
    counted: &mut dyn FnMut(usize),
) -> pango::Layout {
    counted(text.len());
    #[expect(
        clippy::disallowed_methods,
        reason = "the counted constructor; everything else must call this"
    )]
    let layout = pango::Layout::new(context);
    layout.set_text(text);
    layout
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_light_foreground_means_a_dark_scheme() {
        let light_text = Palette::for_foreground(gtk::gdk::RGBA::WHITE);
        let dark_text = Palette::default();
        assert!(light_text.dark);
        assert!(!dark_text.dark);
        assert_eq!(light_text.link, rgba(0x44, 0x93, 0xf8));
        assert_eq!(dark_text.link, rgba(0x09, 0x69, 0xda));
        assert_eq!(light_text.alert(AlertKind::Caution), rgba(0xf8, 0x51, 0x49));
        assert_eq!(dark_text.alert(AlertKind::Tip), rgba(0x1a, 0x7f, 0x37));
    }

    #[test]
    fn faint_keeps_the_colour_and_sets_alpha() {
        let faint = Palette::default().faint(0.25);
        assert!((faint.alpha() - 0.25).abs() < f32::EPSILON);
        assert!(faint.red().abs() < f32::EPSILON);
    }

    #[test]
    fn colour_attributes_add_alpha_only_when_translucent() {
        assert_eq!(foreground(gtk::gdk::RGBA::BLACK).len(), 1);
        assert_eq!(foreground(gtk::gdk::RGBA::new(0.0, 0.0, 0.0, 0.5)).len(), 2);
        assert_eq!(background(gtk::gdk::RGBA::BLACK).len(), 1);
        assert_eq!(background(gtk::gdk::RGBA::new(0.0, 0.0, 0.0, 0.5)).len(), 2);
    }

    #[test]
    fn metrics_follow_size_and_resolution() {
        let setup = TextSetup {
            size_pt: 12.0,
            resolution: 96.0,
            antialias: gtk::cairo::Antialias::Gray,
            hint_style: gtk::cairo::HintStyle::Slight,
            hint_metrics: gtk::cairo::HintMetrics::Off,
            round_glyph_positions: false,
        };
        assert!((setup.metrics().em_px - 16.0).abs() < f64::EPSILON);
        let code = setup.font(FontRole::Code);
        assert_eq!(code.family().as_deref(), Some(CODE_FAMILY));
        assert_eq!(
            code.size(),
            units::to_pango_units(12.0 * FontRole::Code.scale())
        );
    }
}
