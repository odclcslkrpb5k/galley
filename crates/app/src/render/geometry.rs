//! Where a leaf's parts go, in whole logical pixels (docs/plan.md §4.5).
//!
//! One function, `Metrics::geometry`, turns a paint and its text's laid-out height into the
//! leaf's height and the position of everything drawn in it. The height the worker measures and
//! the boxes the widget draws both come from it, so they cannot drift apart.

use super::paint::{BarColor, CODE_PADDING, Decoration, Fill, LeafPaint, Target};
use gbk_markdown_core::model::Align;

use super::table::{CELL_PAD_X_EM, CELL_PAD_Y_EM, Grid, LINE_PX, MIN_COLUMN_EM, column_widths};
use crate::units;

/// Bar width in em, and its least width in pixels.
const BAR_WIDTH_EM: f64 = 0.25;
const BAR_MIN_PX: i64 = 3;
/// Distance of a heading's rule below its text, in em.
const HEADING_RULE_GAP_EM: f64 = 0.25;

/// Lengths in em to pixels, for one body font size.
/// A line of body text's height, in em (Pango's for the body font, as measured in M2).
pub(crate) const LINE_HEIGHT_EM: f64 = 1.4;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Metrics {
    /// The body font's em, in logical pixels.
    pub(crate) em_px: f64,
}

/// A rectangle, in pixels from the leaf's top-left corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rect {
    pub(crate) x: i64,
    pub(crate) y: i64,
    pub(crate) width: i64,
    pub(crate) height: i64,
}

/// What fills a rectangle; the palette picks the colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shade {
    CodeBackground,
    Bar(BarColor),
    Rule,
    HeadingRule,
    TableLine,
    /// Every second body row of a table.
    TableStripe,
}

/// A laid-out leaf: its height, where its text goes, and what is drawn around it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LeafGeometry {
    pub(crate) height: i64,
    /// The top-left corner of the text.
    pub(crate) text_x: i64,
    pub(crate) text_y: i64,
    pub(crate) fills: Vec<(Rect, Shade)>,
    pub(crate) markers: Vec<MarkerPlace>,
    /// A summary's disclosure marker: its right edge and top (the view picks ▾ or ▸).
    pub(crate) disclosure: Option<(i64, i64)>,
    /// For a table: the top-left corner of each cell's text, row by row.
    pub(crate) cells: Vec<Vec<(i64, i64)>>,
}

/// A marker's text, right-aligned to end at `right`, at `top`; and where clicking it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MarkerPlace {
    pub(crate) text: String,
    pub(crate) right: i64,
    pub(crate) top: i64,
    pub(crate) link: Option<Target>,
}

impl Metrics {
    pub(crate) fn px(self, em: f64) -> i64 {
        units::round_px(em * self.em_px)
    }

    /// A leaf's full height given its text's laid-out height.
    pub(crate) fn leaf_height(self, paint: &LeafPaint, text_height: i64) -> i64 {
        self.px(paint.gap_above_em)
            + self.px(paint.pad_above_em)
            + text_height
            + self.px(paint.pad_below_em)
            + self.px(paint.gap_below_em)
    }

    /// Space between a background's left or right edge and the text.
    fn horizontal_padding(self, paint: &LeafPaint) -> i64 {
        if paint.background.is_some() {
            self.px(CODE_PADDING)
        } else {
            0
        }
    }

    /// The width text is wrapped to, given the view's text width.
    pub(crate) fn text_width(self, paint: &LeafPaint, view_text_width: i64) -> i64 {
        (view_text_width - self.px(paint.indent_em) - 2 * self.horizontal_padding(paint)).max(1)
    }

    /// Where everything in a leaf goes, `width` being the view's text width. `text_height` is
    /// the laid-out text's height, or a table's grid's (`grid`).
    pub(crate) fn geometry(
        self,
        paint: &LeafPaint,
        text_height: i64,
        grid: Option<&Grid>,
        width: i64,
    ) -> LeafGeometry {
        let height = self.leaf_height(paint, text_height);
        let gap_above = self.px(paint.gap_above_em);
        let gap_below = self.px(paint.gap_below_em);
        let pad_above = self.px(paint.pad_above_em);
        let indent = self.px(paint.indent_em);
        let text_y = gap_above + pad_above;
        let rest = (width - indent).max(0);
        let mut fills = Vec::new();
        if paint.background == Some(Fill::Code) {
            fills.push((
                Rect {
                    x: indent,
                    y: gap_above,
                    width: rest,
                    height: pad_above + text_height + self.px(paint.pad_below_em),
                },
                Shade::CodeBackground,
            ));
        }
        let mut markers = Vec::new();
        let mut disclosure = None;
        // Markers sit level with the first line of text: a table's is in its first cell.
        let first_line = text_y
            + grid
                .and_then(|grid| grid.cells.first()?.first())
                .map_or(0, |&(_x, y)| y);
        for decoration in &paint.decorations {
            match decoration {
                Decoration::Bar {
                    x_em,
                    color,
                    first,
                    last,
                } => {
                    let top = if *first { gap_above } else { 0 };
                    let bottom = if *last { height - gap_below } else { height };
                    fills.push((
                        Rect {
                            x: self.px(*x_em),
                            y: top,
                            width: self.px(BAR_WIDTH_EM).max(BAR_MIN_PX),
                            height: (bottom - top).max(0),
                        },
                        Shade::Bar(*color),
                    ));
                }
                Decoration::Marker { x_em, text, link } => markers.push(MarkerPlace {
                    text: text.clone(),
                    right: self.px(*x_em),
                    top: first_line,
                    link: link.clone(),
                }),
                Decoration::Disclosure { x_em } => disclosure = Some((self.px(*x_em), first_line)),
                Decoration::Rule => fills.push((
                    Rect {
                        x: indent,
                        y: gap_above + text_height / 2,
                        width: rest,
                        height: 2,
                    },
                    Shade::Rule,
                )),
                Decoration::HeadingRule => fills.push((
                    Rect {
                        x: indent,
                        y: text_y + text_height + self.px(HEADING_RULE_GAP_EM),
                        width: rest,
                        height: 1,
                    },
                    Shade::HeadingRule,
                )),
            }
        }
        // A table narrower than the text width follows the leaf's alignment (`<div
        // align="center">` around it), as text does inside its layout.
        let slack = grid.map_or(0, |grid| (rest - grid.width).max(0));
        let shift = match paint.align {
            Align::Start => 0,
            Align::Center => slack / 2,
            Align::End => slack,
        };
        let text_x = indent + self.horizontal_padding(paint) + shift;
        let cells = grid.map_or_else(Vec::new, |grid| {
            place_grid(grid, (text_x, text_y), &mut fills)
        });
        LeafGeometry {
            height,
            text_x,
            text_y,
            fills,
            markers,
            disclosure,
            cells,
        }
    }

    /// A leaf's height estimated without layout: characters over an average width per line; a
    /// table's rows as their tallest cell's lines, at an even share of the width per column.
    pub(crate) fn estimate(self, paint: &LeafPaint, view_text_width: i64) -> i64 {
        let scale = paint.font.scale();
        let line = units::round_px(self.em_px * scale * LINE_HEIGHT_EM);
        let char_px = self.em_px * scale * 0.5;
        if let Some(table) = paint.table.as_ref() {
            // Columns fitted as `measure` fits them, from the sample's longest lines.
            let pad_x = self.px(CELL_PAD_X_EM);
            let natural: Vec<i64> = table
                .columns
                .chars
                .iter()
                .map(|&chars| units::round_px(units::count_to_f64(chars) * char_px) + 2 * pad_x)
                .collect();
            let widths = column_widths(
                &natural,
                self.text_width(paint, view_text_width),
                self.px(MIN_COLUMN_EM),
            );
            let rows: i64 = table
                .rows
                .iter()
                .map(|row| {
                    let lines = row
                        .cells
                        .iter()
                        .zip(&widths)
                        .map(|(cell, width)| {
                            let per_line = units::px_to_f64(width - 2 * pad_x) / char_px;
                            lines_of(&cell.text, per_line)
                        })
                        .max()
                        .unwrap_or(1);
                    lines * line + 2 * self.px(CELL_PAD_Y_EM) + LINE_PX
                })
                .sum();
            return self.leaf_height(paint, rows + if table.last { LINE_PX } else { 0 });
        }
        let width = units::px_to_f64(self.text_width(paint, view_text_width));
        let lines = lines_of(&paint.text, width / char_px);
        self.leaf_height(paint, lines.max(1) * line)
    }
}

/// Moves a table's grid to `(x, y)` in its leaf: adds its lines and stripes to `fills`, and
/// returns where each cell's text goes.
fn place_grid(
    grid: &Grid,
    (x, y): (i64, i64),
    fills: &mut Vec<(Rect, Shade)>,
) -> Vec<Vec<(i64, i64)>> {
    fills.extend(grid.fills.iter().map(|(rect, shade)| {
        (
            Rect {
                x: rect.x + x,
                y: rect.y + y,
                ..*rect
            },
            *shade,
        )
    }));
    grid.cells
        .iter()
        .map(|row| row.iter().map(|&(left, top)| (left + x, top + y)).collect())
        .collect()
}

/// Lines `text` takes at `per_line` characters to a line: each of its lines, at least one.
fn lines_of(text: &str, per_line: f64) -> i64 {
    let per_line = per_line.max(1.0);
    text.split('\n')
        .map(|line| {
            let chars = units::count_to_f64(line.chars().count());
            units::round_px((chars / per_line).ceil().max(1.0))
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::paint::{FontRole, paint_document};

    fn paints(markdown: &str) -> Vec<LeafPaint> {
        paint_document(&gbk_markdown_core::load(markdown.as_bytes()))
    }

    const METRICS: Metrics = Metrics { em_px: 16.0 };

    #[test]
    fn heights_add_up_and_estimates_grow_with_text() {
        let paints = paints("short\n\n```\ncode\n```\n");
        let short = paints.first().expect("paragraph");
        assert_eq!(METRICS.leaf_height(short, 20), 20 + 12);
        let code = paints.get(1).expect("code");
        assert_eq!(METRICS.leaf_height(code, 20), 10 + 20 + 10 + 12);
        assert_eq!(METRICS.text_width(code, 600), 600 - 20);
        assert_eq!(METRICS.text_width(short, 600), 600);
        assert_eq!(METRICS.text_width(short, 0), 1);
        let long = LeafPaint {
            text: "word ".repeat(400),
            ..short.clone()
        };
        assert!(METRICS.estimate(&long, 600) > 5 * METRICS.estimate(short, 600));
        let lines = LeafPaint {
            text: "a\nb\nc".to_owned(),
            ..short.clone()
        };
        assert_eq!(METRICS.estimate(&lines, 600), 3 * 22 + 12);
    }

    #[test]
    fn the_geometry_height_is_the_leaf_height() {
        for paint in paints("# Title\n\n> quote\n\n- item\n\n```\ncode\n```\n\n---\n") {
            let geometry = METRICS.geometry(&paint, 20, None, 600);
            assert_eq!(geometry.height, METRICS.leaf_height(&paint, 20));
        }
    }

    #[test]
    fn code_text_sits_inside_its_padded_background() {
        let paints = paints("```\ncode\n```\n");
        let geometry = METRICS.geometry(paints.first().expect("code"), 20, None, 600);
        assert_eq!((geometry.text_x, geometry.text_y), (10, 10));
        assert_eq!(
            geometry.fills,
            vec![(
                Rect {
                    x: 0,
                    y: 0,
                    width: 600,
                    height: 40
                },
                Shade::CodeBackground
            )]
        );
        assert_eq!(geometry.height, 52);
    }

    #[test]
    fn bars_stop_at_their_containers_edges() {
        let paints = paints("> # Title\n> body\n\nafter\n");
        let bar = |paint: &LeafPaint| {
            METRICS
                .geometry(paint, 20, None, 600)
                .fills
                .into_iter()
                .find_map(|(rect, shade)| matches!(shade, Shade::Bar(_)).then_some(rect))
                .expect("a bar")
        };
        let title = paints.first().expect("heading");
        // The heading's 1 em gap above is outside the quote; the body's gap below too.
        assert_eq!(bar(title).y, 16);
        assert_eq!(
            bar(title).y + bar(title).height,
            METRICS.leaf_height(title, 20)
        );
        let body = paints.get(1).expect("body");
        assert_eq!(bar(body).y, 0);
        assert_eq!(bar(body).height, 20);
        assert_eq!(bar(body).width, BAR_MIN_PX.max(4));
    }

    #[test]
    fn markers_rules_and_heading_rules_are_placed() {
        let paints = paints("- item\n\n---\n\n# Title\n");
        let item = METRICS.geometry(paints.first().expect("item"), 20, None, 600);
        assert_eq!(
            item.markers,
            vec![MarkerPlace {
                text: "•".to_owned(),
                right: 22,
                top: 0,
                link: None
            }]
        );
        assert_eq!((item.text_x, item.text_y), (28, 0));
        let rule = METRICS.geometry(paints.get(1).expect("rule"), 20, None, 600);
        assert_eq!(
            rule.fills,
            vec![(
                Rect {
                    x: 0,
                    y: 8 + 10,
                    width: 600,
                    height: 2
                },
                Shade::Rule
            )]
        );
        let title = METRICS.geometry(paints.get(2).expect("title"), 40, None, 600);
        assert_eq!(
            title.fills,
            vec![(
                Rect {
                    x: 0,
                    y: 16 + 40 + 4,
                    width: 600,
                    height: 1
                },
                Shade::HeadingRule
            )]
        );
    }

    #[test]
    fn estimates_count_lines_at_an_average_character_width() {
        // Body: 22 px lines (1.4 em), 8 px characters, so 75 to a 600 px line.
        let prose = LeafPaint::plain(&"a".repeat(150), FontRole::Body, Vec::new());
        assert_eq!(METRICS.estimate(&prose, 600), 2 * 22);
        let short = LeafPaint::plain("a", FontRole::Body, Vec::new());
        assert_eq!(METRICS.estimate(&short, 600), 22);
        // Code: 0.85 em, so 19 px lines and 6.8 px characters; 85 to a 580 px line inside the
        // padding, so 160 characters take 2 lines.
        let paints = paints(&format!("```\n{}\n```\n", "c".repeat(160)));
        let code = paints.first().expect("code");
        assert_eq!(METRICS.estimate(code, 600), 10 + 2 * 19 + 10 + 12);
    }

    /// A grid of one row, as `table::grid` would make it for 2 columns of 40 and 60 px.
    fn small_grid(paint: &LeafPaint) -> Grid {
        let table = paint.table.as_ref().expect("a table");
        crate::render::table::grid(table, &[40, 60], &[vec![20, 20]], 13, 6)
    }

    #[test]
    fn a_table_grid_sits_in_its_leaf_and_follows_its_alignment() {
        let paints = paints(
            "> | a | b |\n> |---|---|\n\n<div align=\"center\">\n\n| a | b |\n|---|---|\n\n</div>\n",
        );
        let quoted = paints.first().expect("quoted table");
        let grid = small_grid(quoted);
        let geometry = METRICS.geometry(quoted, grid.height, Some(&grid), 600);
        // Inside the quote's indent; cells offset by it; the marker-free leaf has no disclosure.
        assert_eq!(geometry.text_x, 16);
        assert_eq!(geometry.cells, vec![vec![(16 + 14, 7), (16 + 55, 7)]]);
        assert!(
            geometry
                .fills
                .iter()
                .any(|(rect, shade)| *shade == Shade::TableLine && rect.x == 16)
        );
        assert_eq!(geometry.height, METRICS.leaf_height(quoted, grid.height));
        let centred = paints.get(1).expect("centred table");
        let grid = small_grid(centred);
        let geometry = METRICS.geometry(centred, grid.height, Some(&grid), 600);
        assert_eq!(geometry.text_x, (600 - grid.width) / 2);
    }

    #[test]
    fn table_estimates_wrap_where_the_fitted_column_does() {
        // 16 px em: 8 px characters, 13 px side padding, 6 px top and bottom, 22 px lines. A
        // second column of "b" and "2" is 34 px. 67 characters fit at their natural 562 px
        // (one line); 68 and 70 make the table too wide, so the column shares 597 - 34 = 563
        // px, 67 characters to a line, and they take two.
        let estimate = |chars: usize| {
            let markdown = format!("| a | b |\n|---|---|\n| {} | 2 |\n", "x".repeat(chars));
            METRICS.estimate(paints(&markdown).first().expect("table"), 600)
        };
        let row = |lines: i64| lines * 22 + 12 + 1;
        let one = row(1) + row(1) + 1 + 12;
        let two = row(1) + row(2) + 1 + 12;
        assert_eq!(estimate(67), one);
        assert_eq!(estimate(68), two);
        assert_eq!(estimate(70), two);
    }

    #[test]
    fn a_grid_and_markers_move_with_the_text_origin() {
        let mut item = paints("- | a | b |\n  |---|---|\n")
            .into_iter()
            .next()
            .expect("table in item");
        item.gap_above_em = 1.0;
        let grid = small_grid(&item);
        let geometry = METRICS.geometry(&item, grid.height, Some(&grid), 600);
        let origin = (geometry.text_x, geometry.text_y);
        assert_eq!(origin.1, 16);
        assert_eq!(
            geometry.cells,
            vec![vec![(origin.0 + 14, 16 + 7), (origin.0 + 55, 16 + 7)]]
        );
        let top_line = geometry
            .fills
            .iter()
            .find(|(rect, shade)| *shade == Shade::TableLine && rect.height == 1)
            .map(|(rect, _shade)| (rect.x, rect.y));
        assert_eq!(top_line, Some(origin));
        // The bullet is level with the first cell's text, not the grid's top line.
        assert_eq!(
            geometry.markers.first().map(|marker| marker.top),
            Some(16 + 7)
        );
    }

    #[test]
    fn table_estimates_fit_columns_to_their_sampled_text() {
        let short = paints("| a | b |\n|---|---|\n| 1 | 2 |\n");
        let long_cell = format!("| a | b |\n|---|---|\n| {} | 2 |\n", "word ".repeat(80));
        let long = paints(&long_cell);
        let one = METRICS.estimate(short.first().expect("short"), 600);
        let many = METRICS.estimate(long.first().expect("long"), 600);
        // Two rows of one line each, padding and lines.
        assert_eq!(one, 2 * (22 + 12 + 1) + 1 + 12);
        assert!(many > one + 22, "{many} vs {one}");
    }

    #[test]
    fn indented_leaves_wrap_and_fill_within_their_indent() {
        let paints = paints("- item\n\n  ```\n  code\n  ```\n");
        let item = paints.first().expect("item");
        assert_eq!(METRICS.text_width(item, 600), 600 - 28);
        let code = paints.get(1).expect("code");
        assert_eq!(METRICS.text_width(code, 600), 600 - 28 - 20);
        let fills = METRICS.geometry(code, 20, None, 600).fills;
        let background = fills.first().expect("background").0;
        assert_eq!((background.x, background.width), (28, 600 - 28));
    }
}
