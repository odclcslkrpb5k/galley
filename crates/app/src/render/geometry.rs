//! Where a leaf's parts go, in whole logical pixels (docs/plan.md §4.5).
//!
//! One function, `Metrics::geometry`, turns a paint and its text's laid-out height into the
//! leaf's height and the position of everything drawn in it. The height the worker measures and
//! the boxes the widget draws both come from it, so they cannot drift apart.

use super::paint::{BarColor, CODE_PADDING, Decoration, Fill, LeafPaint, Target};
use super::table::{CELL_PAD_Y_EM, Grid, LINE_PX};
use crate::units;

/// Bar width in em, and its least width in pixels.
const BAR_WIDTH_EM: f64 = 0.25;
const BAR_MIN_PX: i64 = 3;
/// Distance of a heading's rule below its text, in em.
const HEADING_RULE_GAP_EM: f64 = 0.25;

/// Lengths in em to pixels, for one body font size.
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
                    // Level with a table's first line of text, else with the text.
                    top: text_y + grid.map_or(0, |_| LINE_PX + self.px(CELL_PAD_Y_EM)),
                    link: link.clone(),
                }),
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
        let text_x = indent + self.horizontal_padding(paint);
        let mut cells = Vec::new();
        if let Some(grid) = grid {
            fills.extend(grid.fills.iter().map(|(rect, shade)| {
                (
                    Rect {
                        x: rect.x + text_x,
                        y: rect.y + text_y,
                        ..*rect
                    },
                    *shade,
                )
            }));
            cells = grid
                .cells
                .iter()
                .map(|row| row.iter().map(|&(x, y)| (x + text_x, y + text_y)).collect())
                .collect();
        }
        LeafGeometry {
            height,
            text_x,
            text_y,
            fills,
            markers,
            cells,
        }
    }

    /// A leaf's height estimated without layout: characters over an average width per line; a
    /// table's rows as their tallest cell's lines, at an even share of the width per column.
    pub(crate) fn estimate(self, paint: &LeafPaint, view_text_width: i64) -> i64 {
        let scale = paint.font.scale();
        let line = units::round_px(self.em_px * scale * 1.4);
        if let Some(table) = paint.table.as_ref() {
            let columns = units::count_to_f64(table.columns.count.max(1));
            let column = units::px_to_f64(self.text_width(paint, view_text_width)) / columns;
            let per_line = (column / (self.em_px * 0.5)).max(1.0);
            let rows: i64 = table
                .rows
                .iter()
                .map(|row| {
                    let lines = row
                        .cells
                        .iter()
                        .map(|cell| {
                            cell.text
                                .split('\n')
                                .map(|line| {
                                    let chars = units::count_to_f64(line.chars().count());
                                    units::round_px((chars / per_line).ceil().max(1.0))
                                })
                                .sum::<i64>()
                        })
                        .max()
                        .unwrap_or(1);
                    lines * line + 2 * self.px(CELL_PAD_Y_EM) + LINE_PX
                })
                .sum();
            return self.leaf_height(paint, rows + if table.last { LINE_PX } else { 0 });
        }
        let char_px = self.em_px * scale * 0.5;
        let width = units::px_to_f64(self.text_width(paint, view_text_width));
        let per_line = (width / char_px).max(1.0);
        let lines: i64 = paint
            .text
            .split('\n')
            .map(|line| {
                let chars = units::count_to_f64(line.chars().count());
                units::round_px((chars / per_line).ceil().max(1.0))
            })
            .sum();
        self.leaf_height(paint, lines.max(1) * line)
    }
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
