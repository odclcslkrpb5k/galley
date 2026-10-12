//! Turning a `LeafPaint` into a Pango layout (docs/plan.md §4.5).
//!
//! Used on the main thread by the widget and on the height worker, through the same
//! `Typesetter::measure`, so both measure the same height. Colours do not change a layout's
//! size; the worker uses a default palette.

use std::collections::HashMap;

use gtk::pango;
use gtk::prelude::*;

use super::geometry::{LeafGeometry, Metrics};
use super::paint::{AttrKind, FontRole, LeafPaint, LinkSpan, Target, TextAttr};
use super::table::{self, CELL_PAD_X_EM, CELL_PAD_Y_EM, ColumnSample, Grid, MIN_COLUMN_EM};
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

/// A laid-out leaf: its text, or its table's cells and grid; and the height of either.
#[derive(Debug)]
pub(crate) struct Measured {
    pub(crate) content: Content,
    pub(crate) height: i64,
}

#[derive(Debug)]
pub(crate) enum Content {
    Text(pango::Layout),
    Table {
        /// Row by row; a row with fewer cells than columns has empty ones.
        cells: Vec<Vec<pango::Layout>>,
        grid: Grid,
    },
}

impl Measured {
    pub(crate) fn grid(&self) -> Option<&Grid> {
        match &self.content {
            Content::Text(_) => None,
            Content::Table { grid, .. } => Some(grid),
        }
    }
}

/// Each table's column widths, measured once for one view width and text setup: every piece of
/// a table uses them, so its columns line up. Whoever measures keeps one per width.
#[derive(Debug, Default)]
pub(crate) struct ColumnWidths(HashMap<usize, Vec<i64>>);

impl ColumnWidths {
    pub(crate) fn clear(&mut self) {
        self.0.clear();
    }
}

impl Typesetter {
    /// Lays out a leaf for a view `view_text_width` wide. `counted` is told the bytes laid out
    /// (the widget's probe counts them).
    pub(crate) fn measure(
        &self,
        paint: &LeafPaint,
        view_text_width: i64,
        columns: &mut ColumnWidths,
        counted: &mut dyn FnMut(usize),
    ) -> Measured {
        let width = self.setup.metrics().text_width(paint, view_text_width);
        if let Some(table) = paint.table.as_ref() {
            return self.measure_table(table, width, columns, counted);
        }
        let layout = self.layout(
            &paint.text,
            &paint.attrs,
            paint.font,
            paint.align,
            Some(width),
            counted,
        );
        let height = i64::from(layout.pixel_size().1);
        Measured {
            content: Content::Text(layout),
            height,
        }
    }

    fn measure_table(
        &self,
        table: &table::TablePaint,
        width: i64,
        columns: &mut ColumnWidths,
        counted: &mut dyn FnMut(usize),
    ) -> Measured {
        let metrics = self.setup.metrics();
        let (pad_x, pad_y) = (metrics.px(CELL_PAD_X_EM), metrics.px(CELL_PAD_Y_EM));
        let widths = columns
            .0
            .entry(table.columns.table)
            .or_insert_with(|| {
                let natural = self.natural_widths(&table.columns, pad_x, counted);
                table::column_widths(&natural, width, metrics.px(MIN_COLUMN_EM))
            })
            .clone();
        let mut cells = Vec::with_capacity(table.rows.len());
        let mut heights = Vec::with_capacity(table.rows.len());
        for row in &table.rows {
            let mut layouts = Vec::with_capacity(widths.len());
            let mut row_heights = Vec::with_capacity(widths.len());
            for (column, column_width) in widths.iter().enumerate() {
                let layout = match row.cells.get(column) {
                    Some(cell) => self.layout(
                        &cell.text,
                        &cell.attrs,
                        FontRole::Body,
                        cell.align,
                        Some((column_width - 2 * pad_x).max(1)),
                        counted,
                    ),
                    None => self.layout("", &[], FontRole::Body, Align::Start, None, counted),
                };
                row_heights.push(i64::from(layout.pixel_size().1));
                layouts.push(layout);
            }
            cells.push(layouts);
            heights.push(row_heights);
        }
        let grid = table::grid(table, &widths, &heights, pad_x, pad_y);
        Measured {
            height: grid.height,
            content: Content::Table { cells, grid },
        }
    }

    /// Each column's widest sampled cell, unwrapped, with its padding, in whole pixels.
    fn natural_widths(
        &self,
        sample: &ColumnSample,
        pad_x: i64,
        counted: &mut dyn FnMut(usize),
    ) -> Vec<i64> {
        let mut natural = vec![0; sample.count];
        for row in &sample.rows {
            for (cell, widest) in row.cells.iter().zip(natural.iter_mut()) {
                let layout = self.layout(
                    &cell.text,
                    &cell.attrs,
                    FontRole::Body,
                    cell.align,
                    None,
                    counted,
                );
                // Rounded up, so the cell laid out at this width does not wrap.
                let scale = i64::from(pango::SCALE);
                let text = (i64::from(layout.size().0) + scale - 1) / scale;
                *widest = (*widest).max(text + 2 * pad_x);
            }
        }
        natural
    }

    /// The link at `(x, y)` (leaf coordinates) in a leaf laid out as `measured` and placed by
    /// `geometry`: on a linked marker (a footnote's number), or on linked text.
    pub(crate) fn link_at(
        &self,
        paint: &LeafPaint,
        measured: &Measured,
        geometry: &LeafGeometry,
        (x, y): (i64, i64),
    ) -> Option<Target> {
        for marker in &geometry.markers {
            let Some(link) = marker.link.as_ref() else {
                continue;
            };
            let (width, height) = self.marker(&marker.text, &mut |_| {}).pixel_size();
            let (width, height) = (i64::from(width), i64::from(height));
            if (marker.right - width..marker.right).contains(&x)
                && (marker.top..marker.top + height).contains(&y)
            {
                return Some(link.clone());
            }
        }
        match &measured.content {
            Content::Text(layout) => link_in_layout(
                layout,
                &paint.links,
                (x - geometry.text_x, y - geometry.text_y),
            ),
            Content::Table { cells, .. } => {
                let rows = paint
                    .table
                    .as_ref()
                    .map(|table| table.rows.as_slice())
                    .unwrap_or_default();
                cells
                    .iter()
                    .zip(&geometry.cells)
                    .zip(rows)
                    .find_map(|((layouts, origins), row)| {
                        layouts.iter().zip(origins).zip(&row.cells).find_map(
                            |((layout, &(left, top)), cell)| {
                                link_in_layout(layout, &cell.links, (x - left, y - top))
                            },
                        )
                    })
            }
        }
    }

    /// A marker (list bullet, number, summary triangle) in the body font.
    pub(crate) fn marker(&self, text: &str, counted: &mut dyn FnMut(usize)) -> pango::Layout {
        let layout = new_layout(&self.context, text, counted);
        layout.set_font_description(Some(&self.setup.font(FontRole::Body)));
        layout
    }
}

/// The link at `(x, y)` in a layout (layout coordinates), if the point is on its text.
fn link_in_layout(
    layout: &pango::Layout,
    links: &[LinkSpan],
    (x, y): (i64, i64),
) -> Option<Target> {
    if links.is_empty() || x < 0 || y < 0 || y >= i64::from(layout.pixel_size().1) {
        return None;
    }
    let (inside, index, _trailing) = layout.xy_to_index(
        units::to_pango_units(units::px_to_f64(x)),
        units::to_pango_units(units::px_to_f64(y)),
    );
    let index = usize::try_from(index).ok().filter(|_| inside)?;
    links
        .iter()
        .find(|link| (link.start..link.end).contains(&index))
        .map(|link| link.target.clone())
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
        drop(typesetter.measure(sample, 600, &mut ColumnWidths::default(), counted));
    }
}

impl Typesetter {
    /// Lays out text in a font, wrapped at `width` logical pixels (or not at all).
    fn layout(
        &self,
        text: &str,
        text_attrs: &[TextAttr],
        font: FontRole,
        align: Align,
        width: Option<i64>,
        counted: &mut dyn FnMut(usize),
    ) -> pango::Layout {
        let (setup, palette) = (self.setup, self.palette);
        let layout = new_layout(&self.context, text, counted);
        layout.set_font_description(Some(&setup.font(font)));
        if let Some(width) = width {
            layout.set_width(units::to_pango_units(units::px_to_f64(width)));
        }
        layout.set_wrap(pango::WrapMode::WordChar);
        layout.set_alignment(match align {
            Align::Start => pango::Alignment::Left,
            Align::Center => pango::Alignment::Center,
            Align::End => pango::Alignment::Right,
        });
        let attrs = pango::AttrList::new();
        let em = units::to_pango_units(setup.metrics().em_px);
        for attr in text_attrs {
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

    fn typesetter() -> Typesetter {
        use gtk::prelude::FontMapExt as _;
        let setup = TextSetup {
            size_pt: 12.0,
            resolution: 96.0,
            antialias: gtk::cairo::Antialias::Gray,
            hint_style: gtk::cairo::HintStyle::Slight,
            hint_metrics: gtk::cairo::HintMetrics::Off,
            round_glyph_positions: false,
        };
        let context = pangocairo::FontMap::new().create_context();
        setup.apply(&context);
        Typesetter {
            context,
            setup,
            palette: Palette::default(),
        }
    }

    /// Lays out every paint of `markdown` at 600 px.
    fn laid_out(markdown: &str) -> Vec<(LeafPaint, Measured, LeafGeometry)> {
        let typesetter = typesetter();
        let mut columns = ColumnWidths::default();
        crate::render::paint::paint_document(&gbk_markdown_core::load(markdown.as_bytes()))
            .into_iter()
            .map(|paint| {
                let measured = typesetter.measure(&paint, 600, &mut columns, &mut |_| {});
                let geometry = typesetter.setup.metrics().geometry(
                    &paint,
                    measured.height,
                    measured.grid(),
                    600,
                );
                (paint, measured, geometry)
            })
            .collect()
    }

    /// The middle of byte `index`'s character in a layout, in layout coordinates.
    fn middle_of(layout: &pango::Layout, index: usize) -> (i64, i64) {
        let rect = layout.index_to_pos(i32::try_from(index).expect("small"));
        let px = |units: i32| units::round_px(units::from_pango_units(units));
        (
            px(rect.x() + rect.width() / 2),
            px(rect.y() + rect.height() / 2),
        )
    }

    #[test]
    fn links_are_found_under_the_pointer_and_nowhere_else() {
        let typesetter = typesetter();
        let leaves = laid_out("see [the site](https://x.org) here\n");
        let (paint, measured, geometry) = leaves.first().expect("paragraph");
        let Content::Text(layout) = &measured.content else {
            panic!("text");
        };
        let at = |index: usize| {
            let (x, y) = middle_of(layout, index);
            typesetter.link_at(
                paint,
                measured,
                geometry,
                (x + geometry.text_x, y + geometry.text_y),
            )
        };
        let site = Some(Target::Uri("https://x.org".to_owned()));
        assert_eq!(at(paint.text.find("the").expect("link text")), site);
        assert_eq!(at(paint.text.find("site").expect("link text") + 3), site);
        assert_eq!(at(0), None);
        assert_eq!(at(paint.text.find("here").expect("after")), None);
        // Past the end of the line, and outside the leaf.
        assert_eq!(
            typesetter.link_at(paint, measured, geometry, (590, 2)),
            None
        );
        assert_eq!(typesetter.link_at(paint, measured, geometry, (5, -3)), None);
        assert_eq!(
            typesetter.link_at(paint, measured, geometry, (5, 500)),
            None
        );
    }

    #[test]
    fn links_in_table_cells_and_on_footnote_numbers_are_found() {
        let typesetter = typesetter();
        let leaves =
            laid_out("| A | B |\n|---|---|\n| x | [y](#top) |\n\nNote[^1]\n\n[^1]: Text.\n");
        let (paint, measured, geometry) = leaves.first().expect("table");
        let Content::Table { cells, .. } = &measured.content else {
            panic!("a table");
        };
        let cell = cells.get(1).and_then(|row| row.get(1)).expect("cell y");
        let origin = geometry
            .cells
            .get(1)
            .and_then(|row| row.get(1))
            .expect("origin");
        let (x, y) = middle_of(cell, 0);
        assert_eq!(
            typesetter.link_at(paint, measured, geometry, (x + origin.0, y + origin.1)),
            Some(Target::Missing("top".to_owned()))
        );
        let first = geometry
            .cells
            .get(1)
            .and_then(|row| row.first())
            .expect("cell x");
        assert_eq!(
            typesetter.link_at(paint, measured, geometry, (first.0 + 2, first.1 + 2)),
            None
        );
        let (paint, measured, geometry) = leaves.last().expect("footnote");
        let marker = geometry.markers.first().expect("number");
        assert_eq!(
            typesetter.link_at(
                paint,
                measured,
                geometry,
                (marker.right - 2, marker.top + 4)
            ),
            Some(Target::Leaf(1))
        );
        assert_eq!(
            typesetter.link_at(
                paint,
                measured,
                geometry,
                (marker.right + 40, marker.top + 4)
            ),
            None
        );
    }

    #[test]
    fn table_columns_fit_their_text_and_rows_their_tallest_cell() {
        let leaves = laid_out("| Name | Value |\n|---|---|\n| a | b<br>c |\n");
        let (_paint, measured, _geometry) = leaves.first().expect("table");
        let Content::Table { cells, grid } = &measured.content else {
            panic!("a table");
        };
        // Natural widths: the table is narrower than the view, and no header cell wraps.
        assert!(grid.width < 300, "{}", grid.width);
        for layout in cells.first().expect("header") {
            assert_eq!(layout.line_count(), 1);
        }
        let row = cells.get(1).expect("body row");
        let tall = row.get(1).expect("two lines");
        assert_eq!(tall.line_count(), 2);
        // Two rows of padding and lines, plus the tallest cell of each row.
        let one = i64::from(row.first().expect("one line").pixel_size().1);
        let two = i64::from(tall.pixel_size().1);
        let pad = typesetter().setup.metrics().px(CELL_PAD_Y_EM);
        assert_eq!(grid.height, (1 + one + 2 * pad) + (1 + two + 2 * pad) + 1);
    }

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
