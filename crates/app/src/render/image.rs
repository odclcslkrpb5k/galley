//! Images in the text (docs/plan.md §4.6): what each image is, and how big it is drawn. Pure,
//! so it is unit-tested; `images` (GTK side) loads them and `text` reserves their space.
//!
//! An image stands at an object character in its paint's text. Pango reserves a box there the
//! size `display_size` gives: the image's own size once it is decoded (or the `width`/`height`
//! its HTML gives), and before that a placeholder sized from the same attributes or from the
//! alt text. The size is a function of shared data (`ImageTable`), so the height worker and the
//! widget measure the same heights.

use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use galley_core::units::ImageId;

use super::geometry::Metrics;
use crate::units;

/// A placeholder's height, in em: about a line.
const PLACEHOLDER_HEIGHT_EM: f64 = 1.25;
/// A placeholder's width per character of alt text, and around it (room for the ⚠ a failed
/// load adds: a placeholder keeps its size, so nothing moves when a load fails), in em.
const PLACEHOLDER_CHAR_EM: f64 = 0.5;
const PLACEHOLDER_PAD_EM: f64 = 2.0;
/// A placeholder's least width, in em (an image with no alt text).
const PLACEHOLDER_MIN_EM: f64 = 1.5;

/// One image of the document, as the viewer needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImageInfo {
    pub(crate) alt: String,
    /// The `width` and `height` of an HTML `<img>`, in CSS pixels (logical pixels here).
    pub(crate) width: Option<u32>,
    pub(crate) height: Option<u32>,
    pub(crate) source: Option<ImageSource>,
}

/// Where an image comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ImageSource {
    /// An `http:` or `https:` URL.
    Url(String),
    /// A path relative to the document (or, starting with `/`, to its repository).
    Path(String),
}

/// An image's natural size, and the table's version when it arrived.
type Arrived = ((i64, i64), u64);

/// The document's images and the sizes of those decoded so far, shared by the widget and the
/// height worker.
#[derive(Debug, Default)]
pub(crate) struct ImageTable {
    infos: Vec<ImageInfo>,
    /// Natural size, in pixels, of each image decoded, and the version its size arrived at.
    natural: RwLock<Vec<Option<Arrived>>>,
    /// Bumped whenever a size arrives: measurements made before it may be out of date.
    version: AtomicU64,
}

impl ImageTable {
    pub(crate) fn new(infos: Vec<ImageInfo>) -> Self {
        let count = infos.len();
        Self {
            infos,
            natural: RwLock::new(vec![None; count]),
            version: AtomicU64::new(0),
        }
    }

    pub(crate) fn info(&self, id: ImageId) -> Option<&ImageInfo> {
        self.infos.get(id.index())
    }

    pub(crate) fn natural(&self, id: ImageId) -> Option<(i64, i64)> {
        // A poisoned lock means a writer panicked mid-write of a plain Option; the data is
        // still a valid list of sizes.
        let natural = self
            .natural
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        natural
            .get(id.index())
            .copied()
            .flatten()
            .map(|(size, _version)| size)
    }

    /// The latest version at which any of `images` got its size (0 if none has): measurements
    /// of something showing them made at an earlier version are out of date.
    pub(crate) fn arrived(&self, images: &[ImageId]) -> u64 {
        let natural = self
            .natural
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        images
            .iter()
            .filter_map(|id| natural.get(id.index()).copied().flatten())
            .map(|(_size, version)| version)
            .max()
            .unwrap_or(0)
    }

    /// Records an image's decoded size.
    pub(crate) fn set_natural(&self, id: ImageId, size: (i64, i64)) {
        let mut natural = self
            .natural
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Bumped under the lock, so a reader never sees a size without its version.
        let version = self.version.fetch_add(1, Ordering::AcqRel) + 1;
        if let Some(slot) = natural.get_mut(id.index()) {
            *slot = Some((size, version));
        }
    }

    /// Changes whenever a size arrives.
    pub(crate) fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    /// The size an image is drawn at, in logical pixels, inside `max_width` (if any).
    pub(crate) fn display_size(
        &self,
        id: ImageId,
        max_width: Option<i64>,
        metrics: Metrics,
    ) -> (i64, i64) {
        let natural = self.natural(id);
        self.info(id).map_or((1, 1), |info| {
            display_size(info, natural, max_width, metrics)
        })
    }
}

/// The size an image is drawn at: its `width`/`height` attributes (the other side following
/// the image's proportions), else its natural size, else a placeholder; scaled down to fit
/// `max_width`, keeping its proportions.
pub(crate) fn display_size(
    info: &ImageInfo,
    natural: Option<(i64, i64)>,
    max_width: Option<i64>,
    metrics: Metrics,
) -> (i64, i64) {
    let attr = |value: Option<u32>| value.map(i64::from).filter(|&value| value > 0);
    let natural = natural.filter(|&(width, height)| width > 0 && height > 0);
    let (width, height) = match (attr(info.width), attr(info.height), natural) {
        (Some(width), Some(height), _) => (width, height),
        (Some(width), None, Some((nw, nh))) => (width, scale(nh, nh, width, nw).0),
        (None, Some(height), Some((nw, nh))) => (scale(nw, nw, height, nh).0, height),
        (None, None, Some(size)) => size,
        (width, height, None) => {
            let (pw, ph) = placeholder(&info.alt, metrics);
            (width.unwrap_or(pw), height.unwrap_or(ph))
        }
    };
    fit(width.max(1), height.max(1), max_width)
}

/// Images are decoded no larger than this on their longer side (wider than any view).
pub(crate) const MAX_SIDE: i64 = 2048;
/// Most pixels a raster image may declare: more is not decoded (a decompression bomb).
pub(crate) const MAX_PIXELS: i64 = 40_000_000;

/// Whether a raster image declaring `width` × `height` is too big to decode.
pub(crate) fn too_many_pixels(width: i64, height: i64) -> bool {
    width.saturating_mul(height) > MAX_PIXELS
}

/// The size to decode an image of `width` × `height` at, if not its own: the longer side
/// scaled down to `MAX_SIDE`.
pub(crate) fn decode_size(width: i64, height: i64) -> Option<(i64, i64)> {
    let longer = width.max(height);
    (longer > MAX_SIDE).then(|| scale(width, height, MAX_SIDE, longer))
}

/// `width` × `height` scaled by `to / from`, at least 1 × 1.
fn scale(width: i64, height: i64, to: i64, from: i64) -> (i64, i64) {
    let side = |length: i64| {
        i64::try_from(i128::from(length) * i128::from(to) / i128::from(from.max(1)))
            .unwrap_or(to)
            .max(1)
    };
    (side(width), side(height))
}

/// RGB or RGBA pixel rows as premultiplied BGRA (Cairo's own format, so a texture is drawn
/// without converting it each frame).
pub(crate) fn premultiplied_bgra(
    pixels: &[u8],
    (width, height): (usize, usize),
    stride: usize,
    alpha: bool,
) -> Vec<u8> {
    let channels = if alpha { 4 } else { 3 };
    let mut out = Vec::with_capacity(width * height * 4);
    for row in pixels.chunks(stride.max(1)).take(height) {
        for pixel in row.chunks_exact(channels).take(width) {
            let [red, green, blue] = [0, 1, 2].map(|at| pixel.get(at).copied().unwrap_or(0));
            let opacity = if alpha {
                pixel.get(3).copied().unwrap_or(u8::MAX)
            } else {
                u8::MAX
            };
            let times = |channel: u8| {
                u8::try_from(u16::from(channel) * u16::from(opacity) / 255).unwrap_or(u8::MAX)
            };
            out.extend([times(blue), times(green), times(red), opacity]);
        }
    }
    out
}

/// A placeholder's size for alt text: about as wide as the text, a line high.
fn placeholder(alt: &str, metrics: Metrics) -> (i64, i64) {
    let chars = units::count_to_f64(alt.chars().count());
    let width = (chars * PLACEHOLDER_CHAR_EM + PLACEHOLDER_PAD_EM).max(PLACEHOLDER_MIN_EM);
    (metrics.px(width), metrics.px(PLACEHOLDER_HEIGHT_EM))
}

/// Scaled down to `max_width` if wider, keeping proportions.
fn fit(width: i64, height: i64, max_width: Option<i64>) -> (i64, i64) {
    match max_width {
        Some(max) if width > max && max > 0 => scale(width, height, max, width),
        _ => (width, height),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const METRICS: Metrics = Metrics { em_px: 16.0 };

    /// The ids of a one-paragraph document's images (core makes ids only for its own images).
    pub(crate) fn image_ids(markdown: &str) -> Vec<ImageId> {
        let doc = galley_core::load(markdown.as_bytes());
        let mut ids = Vec::new();
        for leaf in doc.leaves() {
            if let galley_core::model::LeafKind::Paragraph(inlines) = leaf.kind() {
                for (_at, object) in inlines.objects() {
                    let galley_core::model::InlineObject::Image(id) = object;
                    ids.push(*id);
                }
            }
        }
        ids
    }

    fn info(width: Option<u32>, height: Option<u32>) -> ImageInfo {
        ImageInfo {
            alt: "Build status".to_owned(),
            width,
            height,
            source: None,
        }
    }

    #[test]
    fn attributes_win_and_the_other_side_follows_the_image() {
        assert_eq!(
            display_size(&info(Some(40), Some(30)), Some((400, 100)), None, METRICS),
            (40, 30)
        );
        assert_eq!(
            display_size(&info(Some(200), None), Some((400, 100)), None, METRICS),
            (200, 50)
        );
        assert_eq!(
            display_size(&info(None, Some(50)), Some((400, 100)), None, METRICS),
            (200, 50)
        );
        assert_eq!(
            display_size(&info(None, None), Some((400, 100)), None, METRICS),
            (400, 100)
        );
    }

    #[test]
    fn before_decoding_a_placeholder_takes_what_the_attributes_say() {
        // "Build status": 12 characters at 8 px, plus 32 px; 20 px high.
        assert_eq!(
            display_size(&info(None, None), None, None, METRICS),
            (128, 20)
        );
        assert_eq!(
            display_size(&info(Some(300), None), None, None, METRICS),
            (300, 20)
        );
        assert_eq!(
            display_size(&info(None, Some(90)), None, None, METRICS),
            (128, 90)
        );
        let empty = ImageInfo {
            alt: String::new(),
            ..info(None, None)
        };
        assert_eq!(display_size(&empty, None, None, METRICS), (32, 20));
        // A natural size with no height is no size.
        assert_eq!(
            display_size(&info(None, None), Some((100, 0)), None, METRICS),
            (128, 20)
        );
        // Zero attributes and sizes are ignored.
        assert_eq!(
            display_size(&info(Some(0), Some(0)), Some((0, 5)), None, METRICS),
            (128, 20)
        );
    }

    #[test]
    fn wide_images_shrink_to_fit_keeping_their_proportions() {
        assert_eq!(
            display_size(&info(None, None), Some((1200, 600)), Some(600), METRICS),
            (600, 300)
        );
        assert_eq!(
            display_size(&info(None, None), Some((600, 600)), Some(600), METRICS),
            (600, 600)
        );
        assert_eq!(
            display_size(&info(None, None), Some((1200, 1)), Some(600), METRICS),
            (600, 1)
        );
        assert_eq!(fit(50, 50, Some(0)), (50, 50));
    }

    #[test]
    fn decoding_scales_the_longer_side_and_bombs_are_too_big() {
        assert_eq!(decode_size(100, 50), None);
        assert_eq!(decode_size(2048, 2048), None);
        assert_eq!(decode_size(4096, 1024), Some((2048, 512)));
        assert_eq!(decode_size(1000, 8192), Some((250, 2048)));
        assert_eq!(decode_size(100_000, 1), Some((2048, 1)));
        assert!(!too_many_pixels(8000, 5000));
        assert!(too_many_pixels(40_000_001, 1));
        assert!(too_many_pixels(i64::MAX, i64::MAX));
    }

    #[test]
    fn pixels_become_premultiplied_bgra() {
        // Two RGBA pixels (half-transparent red, opaque blue) in a row padded to 12 bytes.
        let rgba = [255, 0, 0, 128, 0, 0, 255, 255, 9, 9, 9, 9];
        assert_eq!(
            premultiplied_bgra(&rgba, (2, 1), 12, true),
            vec![0, 0, 128, 128, 255, 0, 0, 255]
        );
        // RGB: opaque.
        let rgb = [10, 20, 30, 40, 50, 60];
        assert_eq!(
            premultiplied_bgra(&rgb, (1, 2), 3, false),
            vec![30, 20, 10, 255, 60, 50, 40, 255]
        );
    }

    #[test]
    fn huge_attributes_do_not_overflow() {
        let big = info(Some(u32::MAX), None);
        assert_eq!(
            display_size(&big, Some((1, i64::from(u32::MAX))), Some(800), METRICS),
            (800, 800)
        );
    }

    #[test]
    fn the_table_shares_sizes_and_counts_arrivals() {
        let ids = image_ids("![a](x.png) ![b](y.png)");
        let (first, second) = (ids[0], ids[1]);
        let table = ImageTable::new(vec![info(None, None), info(Some(10), None)]);
        assert_eq!(table.version(), 0);
        assert_eq!(table.natural(first), None);
        assert_eq!(table.display_size(second, None, METRICS), (10, 20));
        assert_eq!(table.arrived(&[first, second]), 0);
        table.set_natural(second, (100, 50));
        assert_eq!(table.version(), 1);
        assert_eq!(table.arrived(&[first, second]), 1);
        assert_eq!(table.arrived(&[first]), 0);
        assert_eq!(table.natural(second), Some((100, 50)));
        assert_eq!(table.display_size(second, None, METRICS), (10, 5));
        assert_eq!(table.natural(first), None);
    }
}
