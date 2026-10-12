//! Tables as grids (docs/plan.md §4.5): what a table piece holds, how its columns share the
//! width, and where its rows, cells and lines go. Pure, so it is unit-tested; `text` lays out the
//! cells and `geometry` places the grid in its leaf.
//!
//! A table is painted in pieces of a few rows (`paint`), each a leaf of its own, so a long table
//! never has to be laid out whole. Every piece of one table shares a `ColumnSample`: its header
//! and first rows, from which the column widths are measured once per width. Rows after the
//! sample that are wider wrap instead of widening the table, so pieces never disagree.

use std::sync::Arc;

use gbk_markdown_core::model::Align;
use gbk_markdown_core::units::TableId;

use super::geometry::{Rect, Shade};
use super::paint::{LinkSpan, TextAttr};

/// Space between a cell's lines and its text, in em: GitHub's 6 px by 13 px at 16 px.
pub(crate) const CELL_PAD_Y_EM: f64 = 0.375;
pub(crate) const CELL_PAD_X_EM: f64 = 0.8125;
/// The narrowest a column is squeezed to, in em, however many columns share the width.
pub(crate) const MIN_COLUMN_EM: f64 = 3.0;
/// Width of the grid's lines, in pixels.
pub(crate) const LINE_PX: i64 = 1;

/// One piece of a table: some of its rows.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TablePaint {
    pub(crate) columns: Arc<ColumnSample>,
    pub(crate) rows: Vec<RowPaint>,
    /// Whether this piece ends the table, and so draws its bottom line.
    pub(crate) last: bool,
}

/// The rows a table's column widths are measured from, shared by all its pieces.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ColumnSample {
    /// Which table of the document: the key its measured widths are kept under.
    pub(crate) table: TableId,
    pub(crate) count: usize,
    pub(crate) rows: Vec<RowPaint>,
    /// Each column's longest sampled line, in characters: for estimating heights before any
    /// layout.
    pub(crate) chars: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RowPaint {
    pub(crate) cells: Vec<CellPaint>,
    /// A body row with a shaded background (every second one, as on GitHub).
    pub(crate) striped: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CellPaint {
    pub(crate) text: String,
    pub(crate) attrs: Vec<TextAttr>,
    pub(crate) links: Vec<LinkSpan>,
    pub(crate) align: Align,
}

/// Column widths, line to line, for `available` pixels, given each column's natural width
/// (its widest sampled cell, unwrapped, padding included). A table that fits keeps its natural
/// widths, as on GitHub. One that does not shares the width out: narrow columns keep their
/// natural width, wide ones split what is left, none narrower than `min` (so a table with many
/// columns may still be wider than the view).
pub(crate) fn column_widths(natural: &[i64], available: i64, min: i64) -> Vec<i64> {
    let lines = LINE_PX * (i64::try_from(natural.len()).unwrap_or(i64::MAX) + 1);
    let room = (available - lines).max(0);
    if natural.iter().sum::<i64>() <= room {
        return natural.to_vec();
    }
    let mut order: Vec<usize> = (0..natural.len()).collect();
    order.sort_by_key(|&column| natural.get(column).copied().unwrap_or_default());
    let mut widths = vec![0; natural.len()];
    let mut left = room;
    for (done, &column) in order.iter().enumerate() {
        let sharing = i64::try_from(order.len() - done).unwrap_or(1).max(1);
        let natural = natural.get(column).copied().unwrap_or_default();
        // Never wider than its natural width, nor narrower than `min` unless that is wider.
        let width = natural.min((left / sharing).max(min));
        if let Some(slot) = widths.get_mut(column) {
            *slot = width;
        }
        left = (left - width).max(0);
    }
    widths
}

/// Where a table piece's rows, cells and lines go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Grid {
    pub(crate) width: i64,
    pub(crate) height: i64,
    /// The top-left corner of each cell's text, row by row.
    pub(crate) cells: Vec<Vec<(i64, i64)>>,
    pub(crate) fills: Vec<(Rect, Shade)>,
}

/// The grid of `table` with column `widths` and each cell's text height (`heights`, row by row),
/// `pad_x` and `pad_y` pixels inside each cell. Every row has a line above it, the table's last
/// piece one below too; each column boundary is one upright the height of the piece.
pub(crate) fn grid(
    table: &TablePaint,
    widths: &[i64],
    heights: &[Vec<i64>],
    pad_x: i64,
    pad_y: i64,
) -> Grid {
    let width = widths.iter().map(|width| width + LINE_PX).sum::<i64>() + LINE_PX;
    let mut cells = Vec::with_capacity(table.rows.len());
    let mut fills = Vec::new();
    let line = |y: i64| {
        (
            Rect {
                x: 0,
                y,
                width,
                height: LINE_PX,
            },
            Shade::TableLine,
        )
    };
    let mut y = 0;
    for (row, row_heights) in table.rows.iter().zip(heights) {
        let height = row_heights.iter().copied().max().unwrap_or(0) + 2 * pad_y;
        fills.push(line(y));
        y += LINE_PX;
        if row.striped {
            fills.push((
                Rect {
                    x: 0,
                    y,
                    width,
                    height,
                },
                Shade::TableStripe,
            ));
        }
        let mut x = LINE_PX;
        cells.push(
            widths
                .iter()
                .map(|column_width| {
                    let origin = (x + pad_x, y + pad_y);
                    x += column_width + LINE_PX;
                    origin
                })
                .collect(),
        );
        y += height;
    }
    if table.last {
        fills.push(line(y));
        y += LINE_PX;
    }
    let mut x = 0;
    for column_width in widths.iter().copied().chain([0]) {
        fills.push((
            Rect {
                x,
                y: 0,
                width: LINE_PX,
                height: y,
            },
            Shade::TableLine,
        ));
        x += column_width + LINE_PX;
    }
    Grid {
        width,
        height: y,
        cells,
        fills,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A table id (core makes them only for its own tables).
    fn table_id() -> TableId {
        let doc = gbk_markdown_core::load(b"| a |\n|---|\n");
        doc.leaves()
            .iter()
            .find_map(|leaf| {
                if let gbk_markdown_core::model::LeafKind::TableRows { table, .. } = leaf.kind() {
                    Some(*table)
                } else {
                    None
                }
            })
            .expect("a table")
    }

    fn cell(text: &str) -> CellPaint {
        CellPaint {
            text: text.to_owned(),
            attrs: Vec::new(),
            links: Vec::new(),
            align: Align::Start,
        }
    }

    fn table(rows: &[(&[&str], bool)], last: bool) -> TablePaint {
        let rows: Vec<RowPaint> = rows
            .iter()
            .map(|(cells, striped)| RowPaint {
                cells: cells.iter().map(|text| cell(text)).collect(),
                striped: *striped,
            })
            .collect();
        TablePaint {
            columns: Arc::new(ColumnSample {
                table: table_id(),
                count: 2,
                rows: rows.clone(),
                chars: vec![1, 1],
            }),
            rows,
            last,
        }
    }

    #[test]
    fn a_table_that_fits_keeps_its_natural_widths() {
        assert_eq!(column_widths(&[50, 120], 600, 48), vec![50, 120]);
        // Exactly fitting: 3 lines of 1 px.
        assert_eq!(column_widths(&[100, 97], 200, 48), vec![100, 97]);
    }

    #[test]
    fn a_wide_table_gives_narrow_columns_their_width_and_shares_the_rest() {
        // 300 px less 4 lines: the 60 px column keeps its width, the others split 236.
        assert_eq!(column_widths(&[400, 60, 500], 300, 48), vec![118, 60, 118]);
        // No column narrower than the least, even if the table then overflows; but a column
        // narrower than the least by nature keeps its own width.
        assert_eq!(column_widths(&[400, 400, 400], 100, 48), vec![48, 48, 48]);
        assert_eq!(column_widths(&[20, 400, 400], 100, 48), vec![20, 48, 48]);
        assert_eq!(column_widths(&[], 100, 48), Vec::<i64>::new());
    }

    #[test]
    fn rows_are_as_tall_as_their_tallest_cell_and_lines_frame_them() {
        let piece = table(&[(&["a", "b"], false), (&["c", "d"], true)], true);
        let grid = grid(&piece, &[40, 60], &[vec![20, 40], vec![20, 20]], 13, 6);
        assert_eq!(grid.width, 1 + 40 + 1 + 60 + 1);
        // Rows of 40 + 12 and 20 + 12, a line above each and one below the last.
        assert_eq!(grid.height, 1 + 52 + 1 + 32 + 1);
        assert_eq!(
            grid.cells,
            vec![vec![(14, 7), (55, 7)], vec![(14, 60), (55, 60)]]
        );
        let stripes: Vec<Rect> = grid
            .fills
            .iter()
            .filter(|(_rect, shade)| *shade == Shade::TableStripe)
            .map(|(rect, _shade)| *rect)
            .collect();
        assert_eq!(
            stripes,
            vec![Rect {
                x: 0,
                y: 54,
                width: 103,
                height: 32
            }]
        );
        let lines = grid
            .fills
            .iter()
            .filter(|(_rect, shade)| *shade == Shade::TableLine)
            .count();
        // A line above each row and below the last; three uprights the height of the piece.
        assert_eq!(lines, 2 + 1 + 3);
        let upright = grid
            .fills
            .iter()
            .filter(|(rect, shade)| *shade == Shade::TableLine && rect.width == 1)
            .map(|(rect, _shade)| (rect.x, rect.y, rect.height))
            .collect::<Vec<_>>();
        assert_eq!(
            upright,
            vec![
                (0, 0, grid.height),
                (41, 0, grid.height),
                (102, 0, grid.height)
            ]
        );
    }

    #[test]
    fn only_the_last_piece_closes_the_table() {
        let piece = table(&[(&["a"], false)], false);
        let grid = grid(&piece, &[40], &[vec![20]], 0, 0);
        assert_eq!(grid.height, 1 + 20);
        assert_eq!(grid.fills.len(), 1 + 2);
        assert!(
            grid.fills
                .iter()
                .all(|(rect, _shade)| rect.y + rect.height <= grid.height)
        );
    }
}
