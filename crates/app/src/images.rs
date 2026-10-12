//! Loading images off the main thread (docs/plan.md §4.6).
//!
//! The view asks for an image once its placeholder has been drawn (so nothing is read or
//! fetched before the first frame). A few threads, started on the first request, read local
//! files or fetch `http(s)` URLs with a timeout and a size cap, decode with gdk-pixbuf (any
//! format its loaders know: PNG, JPEG, GIF's first frame, SVG, WebP...), and hand back a
//! `gdk::Texture`, which GDK allows to cross threads. The view collects results between
//! frames.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use gbk_markdown_core::units::ImageId;
use gtk::gdk;
use gtk::gdk_pixbuf;
use gtk::prelude::*;

use crate::links;
use crate::render::image::{ImageInfo, ImageSource, ImageTable};
use crate::render::paint::LeafPaint;

/// Threads loading at once: enough for a README's row of badges, few enough to be polite.
const THREADS: usize = 4;
/// Most bytes read or fetched for one image.
const MAX_BYTES: u64 = 10 * 1024 * 1024;
/// A remote image that takes longer than this, redirects included, fails.
const TIMEOUT: Duration = Duration::from_secs(20);
const MAX_REDIRECTS: u32 = 5;
/// Images are decoded no larger than this on their longer side (wider than any view), and
/// refused if they declare more pixels than `MAX_PIXELS` (a decompression bomb).
const MAX_SIDE: i32 = 2048;
const MAX_PIXELS: i64 = 40_000_000;
/// gdk-pixbuf's built-in raster loaders, which decode in this process.
const IN_PROCESS: [&str; 12] = [
    "png", "jpeg", "gif", "bmp", "ico", "ani", "tiff", "tga", "pnm", "xpm", "xbm", "qtif",
];
/// Bytes given to the decoder at a time, so a refused size stops reading early.
const DECODE_CHUNK: usize = 16 * 1024;

/// What the view has of an image.
#[derive(Debug, Clone)]
pub(crate) enum ImageState {
    /// Not asked for yet: its placeholder has not been drawn.
    Waiting,
    Loading,
    Ready(gdk::Texture),
    Failed(ImageError),
    /// Not loaded, by choice: `--offline` for a remote image, or nowhere to load it from.
    NotLoaded,
}

/// The view's images: the shared table of sizes, what each image is now, and the loads in
/// flight.
#[derive(Debug)]
pub(crate) struct Gallery {
    table: Arc<ImageTable>,
    states: Vec<ImageState>,
    loader: Loader,
    /// The paints each image is in, to redraw when it arrives.
    paints: HashMap<ImageId, Vec<usize>>,
    /// The document's file, which relative paths start from.
    document: Option<PathBuf>,
    /// Whether remote images are fetched (not with `--offline`).
    remote: bool,
    /// Whether images are loaded at all (not while `--check-heights` compares layouts).
    enabled: bool,
}

/// How images may be loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Loading {
    pub(crate) remote: bool,
    pub(crate) enabled: bool,
}

impl Gallery {
    pub(crate) fn new(
        infos: Vec<ImageInfo>,
        paints: &[LeafPaint],
        document: Option<PathBuf>,
        loading: Loading,
    ) -> Self {
        let mut by_image: HashMap<ImageId, Vec<usize>> = HashMap::new();
        for (index, paint) in paints.iter().enumerate() {
            for image in &paint.images {
                by_image.entry(*image).or_default().push(index);
            }
        }
        let count = infos.len();
        Self {
            table: Arc::new(ImageTable::new(infos)),
            states: vec![ImageState::Waiting; count],
            loader: Loader::default(),
            paints: by_image,
            document,
            remote: loading.remote,
            enabled: loading.enabled,
        }
    }

    pub(crate) fn table(&self) -> &Arc<ImageTable> {
        &self.table
    }

    pub(crate) fn state(&self, image: ImageId) -> Option<&ImageState> {
        self.states.get(image.index())
    }

    /// Asks for a drawn paint's images that are still waiting.
    pub(crate) fn request(&mut self, paint: &LeafPaint) {
        if !self.enabled {
            return;
        }
        for &image in &paint.images {
            if !matches!(self.states.get(image.index()), Some(ImageState::Waiting)) {
                continue;
            }
            let fetch = self.fetch(image);
            let state = match fetch {
                Some(fetch) => {
                    self.loader.request(image, fetch);
                    ImageState::Loading
                }
                None => ImageState::NotLoaded,
            };
            if let Some(slot) = self.states.get_mut(image.index()) {
                *slot = state;
            }
        }
    }

    /// Where an image would be loaded from, if it may be.
    fn fetch(&self, image: ImageId) -> Option<Fetch> {
        match self.table.info(image)?.source.as_ref()? {
            ImageSource::Url(url) => self.remote.then(|| Fetch::Url(url.clone())),
            ImageSource::Path(path) => {
                let is_repository = |folder: &std::path::Path| folder.join(".git").exists();
                let document = self.document.as_deref()?;
                Some(Fetch::File(links::resolve_path(
                    document,
                    path,
                    &is_repository,
                )))
            }
        }
    }

    /// Takes the loads finished since last asked; returns the paints that show them.
    pub(crate) fn collect(&mut self) -> Vec<usize> {
        let mut changed = Vec::new();
        for Loaded { image, result } in self.loader.ready() {
            let state = match result {
                Ok(texture) => {
                    self.table.set_natural(
                        image,
                        (i64::from(texture.width()), i64::from(texture.height())),
                    );
                    ImageState::Ready(texture)
                }
                Err(error) => ImageState::Failed(error),
            };
            if let Some(slot) = self.states.get_mut(image.index()) {
                *slot = state;
            }
            changed.extend(self.paints.get(&image).into_iter().flatten().copied());
        }
        changed.sort_unstable();
        changed.dedup();
        changed
    }

    /// Loads in flight.
    pub(crate) fn outstanding(&self) -> usize {
        self.loader.outstanding()
    }

    /// For the probe: images shown, failed, and still loading.
    pub(crate) fn counts(&self) -> (usize, usize, usize) {
        let count =
            |wanted: fn(&ImageState) -> bool| self.states.iter().filter(|s| wanted(s)).count();
        (
            count(|state| matches!(state, ImageState::Ready(_))),
            count(|state| matches!(state, ImageState::Failed(_))),
            count(|state| matches!(state, ImageState::Loading)),
        )
    }
}

/// Where to load an image from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Fetch {
    Url(String),
    File(PathBuf),
}

/// Why an image could not be shown.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ImageError {
    #[error("cannot read {0}: {1:?}")]
    Read(PathBuf, std::io::ErrorKind),
    #[error("{0} is not a file")]
    NotFile(PathBuf),
    #[error("larger than {MAX_BYTES} bytes")]
    TooLarge,
    #[error("cannot fetch it: {0}")]
    Fetch(String),
    #[error("cannot decode it: {0}")]
    Decode(String),
    #[error("declares {0} × {1} pixels, more than shown")]
    TooManyPixels(i32, i32),
    #[error("the image loader stopped")]
    LoaderGone,
}

/// A finished load.
#[derive(Debug)]
pub(crate) struct Loaded {
    pub(crate) image: ImageId,
    pub(crate) result: Result<gdk::Texture, ImageError>,
}

type Request = (ImageId, Fetch);

/// The loading threads, started on the first request.
#[derive(Debug)]
pub(crate) struct Loader {
    requests: Option<Sender<Request>>,
    results: Receiver<Loaded>,
    results_sender: Sender<Loaded>,
    /// Requests made and not yet collected.
    outstanding: Rc<Cell<usize>>,
}

impl Default for Loader {
    fn default() -> Self {
        let (results_sender, results) = mpsc::channel();
        Self {
            requests: None,
            results,
            results_sender,
            outstanding: Rc::new(Cell::new(0)),
        }
    }
}

impl Loader {
    /// Asks for an image; the result comes from `ready`.
    pub(crate) fn request(&mut self, image: ImageId, fetch: Fetch) {
        let requests = self
            .requests
            .get_or_insert_with(|| start(&self.results_sender));
        self.outstanding.set(self.outstanding.get() + 1);
        if requests.send((image, fetch)).is_err() {
            // Every thread has stopped: say so for this image, through the usual path.
            drop(self.results_sender.send(Loaded {
                image,
                result: Err(ImageError::LoaderGone),
            }));
        }
    }

    /// The loads finished since last asked.
    pub(crate) fn ready(&self) -> Vec<Loaded> {
        let ready: Vec<Loaded> = self.results.try_iter().collect();
        self.outstanding
            .set(self.outstanding.get().saturating_sub(ready.len()));
        ready
    }

    /// Requests not yet collected.
    pub(crate) fn outstanding(&self) -> usize {
        self.outstanding.get()
    }
}

/// Starts the threads; returns where to send requests.
fn start(results: &Sender<Loaded>) -> Sender<Request> {
    let (requests, queue) = mpsc::channel::<Request>();
    let queue = Arc::new(Mutex::new(queue));
    let agent = Arc::new(agent());
    for _ in 0..THREADS {
        let queue = Arc::clone(&queue);
        let results = results.clone();
        let agent = Arc::clone(&agent);
        thread::spawn(move || {
            loop {
                // One thread waits on the queue at a time; the lock is released before loading.
                let next = queue
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .recv();
                let Ok((image, fetch)) = next else {
                    return;
                };
                let result = load(&agent, &fetch);
                if results.send(Loaded { image, result }).is_err() {
                    return;
                }
            }
        });
    }
    requests
}

/// An HTTP client: rustls with ring, certificates checked against the system's store.
fn agent() -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::Rustls)
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .unversioned_rustls_crypto_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .build();
    ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .max_redirects(MAX_REDIRECTS)
        .user_agent(concat!("gbk-markdown/", env!("CARGO_PKG_VERSION")))
        .tls_config(tls)
        .build()
        .new_agent()
}

fn load(agent: &ureq::Agent, fetch: &Fetch) -> Result<gdk::Texture, ImageError> {
    let bytes = match fetch {
        Fetch::File(path) => read(path)?,
        Fetch::Url(url) => {
            let mut response = agent
                .get(url)
                .call()
                .map_err(|error| ImageError::Fetch(error.to_string()))?;
            response
                .body_mut()
                .with_config()
                .limit(MAX_BYTES)
                .read_to_vec()
                .map_err(|error| ImageError::Fetch(error.to_string()))?
        }
    };
    decode(&bytes)
}

fn read(path: &PathBuf) -> Result<Vec<u8>, ImageError> {
    let metadata =
        std::fs::metadata(path).map_err(|error| ImageError::Read(path.clone(), error.kind()))?;
    if !metadata.is_file() {
        return Err(ImageError::NotFile(path.clone()));
    }
    if metadata.len() > MAX_BYTES {
        return Err(ImageError::TooLarge);
    }
    std::fs::read(path).map_err(|error| ImageError::Read(path.clone(), error.kind()))
}

/// Decodes any format gdk-pixbuf knows, no larger than `MAX_SIDE`.
pub(crate) fn decode(bytes: &[u8]) -> Result<gdk::Texture, ImageError> {
    let loader = gdk_pixbuf::PixbufLoader::new();
    let refused = Rc::new(Cell::new(None));
    let seen = Rc::clone(&refused);
    // Called once the format's header gives the size, before any pixels are allocated. A size
    // of 0 × 0 tells gdk-pixbuf's own loaders to stop and allocate nothing (its contract for the
    // size callback): a small PNG declaring billions of pixels is refused there. Formats decoded
    // out of process by glycin (SVG, HEIF, JPEG XL on Ubuntu 26.04) are sandboxed, and glycin
    // waits forever on a 0 × 0 size (seen in M4), so those are only scaled down.
    loader.connect_size_prepared(move |loader, width, height| {
        let in_process = loader
            .format()
            .and_then(|format| format.name())
            .is_some_and(|name| IN_PROCESS.contains(&name.as_str()));
        if in_process && i64::from(width) * i64::from(height) > MAX_PIXELS {
            seen.set(Some((width, height)));
            loader.set_size(0, 0);
            return;
        }
        let (fitted_width, fitted_height) = fit_side(width, height);
        if (fitted_width, fitted_height) != (width, height) {
            loader.set_size(fitted_width, fitted_height);
        }
    });
    let decode_error = |error: gtk::glib::Error| ImageError::Decode(error.to_string());
    let refusal = |refused: &Cell<Option<(i32, i32)>>| {
        refused
            .get()
            .map(|(width, height)| ImageError::TooManyPixels(width, height))
    };
    for chunk in bytes.chunks(DECODE_CHUNK) {
        let written = loader.write(chunk);
        if let Some(error) = refusal(&refused) {
            drop(loader.close());
            return Err(error);
        }
        written.map_err(decode_error)?;
    }
    let closed = loader.close();
    if let Some(error) = refusal(&refused) {
        return Err(error);
    }
    closed.map_err(decode_error)?;
    let pixbuf = loader
        .pixbuf()
        .ok_or_else(|| ImageError::Decode("no image in the data".to_owned()))?;
    Ok(texture(&pixbuf))
}

/// The size to decode at: as given, or scaled down so the longer side is `MAX_SIDE`.
fn fit_side(width: i32, height: i32) -> (i32, i32) {
    let longer = width.max(height);
    if longer <= MAX_SIDE || longer <= 0 {
        return (width, height);
    }
    let scale = |side: i32| {
        i32::try_from(i64::from(side) * i64::from(MAX_SIDE) / i64::from(longer))
            .unwrap_or(MAX_SIDE)
            .max(1)
    };
    (scale(width), scale(height))
}

/// A pixbuf's pixels as a texture.
fn texture(pixbuf: &gdk_pixbuf::Pixbuf) -> gdk::Texture {
    let format = if pixbuf.has_alpha() {
        gdk::MemoryFormat::R8g8b8a8
    } else {
        gdk::MemoryFormat::R8g8b8
    };
    let stride = usize::try_from(pixbuf.rowstride()).unwrap_or(0);
    gdk::MemoryTexture::new(
        pixbuf.width(),
        pixbuf.height(),
        format,
        &pixbuf.read_pixel_bytes(),
        stride,
    )
    .upcast()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2 × 1 PNG, red then blue, written by hand.
    fn png() -> Vec<u8> {
        let pixbuf =
            gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, false, 8, 2, 1).expect("a pixbuf");
        pixbuf.put_pixel(0, 0, 255, 0, 0, 255);
        pixbuf.put_pixel(1, 0, 0, 0, 255, 255);
        pixbuf.save_to_bufferv("png", &[]).expect("encoded")
    }

    #[test]
    fn images_decode_to_their_size() {
        let texture = decode(&png()).expect("decoded");
        assert_eq!((texture.width(), texture.height()), (2, 1));
    }

    #[test]
    fn rubbish_and_bombs_are_errors() {
        assert!(matches!(
            decode(b"not an image"),
            Err(ImageError::Decode(_))
        ));
        assert!(matches!(decode(b""), Err(ImageError::Decode(_))));
        // An SVG declaring 100 000 × 100 000 pixels is drawn no larger than the cap.
        let huge =
            br#"<svg xmlns="http://www.w3.org/2000/svg" width="100000" height="50000"></svg>"#;
        let texture = decode(huge).expect("an SVG");
        assert_eq!((texture.width(), texture.height()), (2048, 1024));
    }

    #[test]
    fn a_small_png_declaring_too_many_pixels_is_refused() {
        // A PNG whose header says 20 000 × 20 000 (400 megapixels), in a few bytes: the size
        // is refused before any pixels are allocated.
        let mut png = png();
        // IHDR's width and height are bytes 16..24.
        png.splice(
            16..24,
            [20_000_u32.to_be_bytes(), 20_000_u32.to_be_bytes()].concat(),
        );
        // The header's checksum no longer matches; recompute it (bytes 29..33 cover 12..29).
        let crc = crc32(png.get(12..29).expect("IHDR"));
        png.splice(29..33, crc.to_be_bytes());
        assert_eq!(
            decode(&png).map(|_| ()),
            Err(ImageError::TooManyPixels(20_000, 20_000))
        );
    }

    /// The CRC-32 PNG chunks carry.
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0_u32;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    #[test]
    fn the_gallery_loads_drawn_images_once_and_reports_their_paints() {
        let dir = std::env::temp_dir().join(format!("gbk-gallery-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join("dot.png"), png()).expect("write");
        let markdown = "![a](dot.png) ![b](https://x.org/b.png) ![c](missing.png) ![d](#x)\n";
        let doc = gbk_markdown_core::load(markdown.as_bytes());
        let paints = crate::render::paint::paint_document(&doc);
        let infos = crate::render::paint::image_infos(&doc);
        let loading = Loading {
            remote: false,
            enabled: true,
        };
        let mut gallery = Gallery::new(infos, &paints, Some(dir.join("doc.md")), loading);
        let paint = paints.first().expect("paragraph");
        let [a, b, c, d] = paint.images.as_slice() else {
            panic!("four images");
        };
        assert!(matches!(gallery.state(*a), Some(ImageState::Waiting)));
        gallery.request(paint);
        gallery.request(paint);
        // Only the local ones load: b is remote (off) and d has no source.
        assert_eq!(gallery.outstanding(), 2);
        assert!(matches!(gallery.state(*b), Some(ImageState::NotLoaded)));
        assert!(matches!(gallery.state(*d), Some(ImageState::NotLoaded)));
        let mut changed = Vec::new();
        for _ in 0..500 {
            changed.extend(gallery.collect());
            if gallery.outstanding() == 0 {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        // Both loads are in paint 0 (one `collect` or two, depending on timing).
        changed.dedup();
        assert_eq!(changed, vec![0]);
        assert!(matches!(gallery.state(*a), Some(ImageState::Ready(_))));
        assert!(matches!(
            gallery.state(*c),
            Some(ImageState::Failed(ImageError::Read(..)))
        ));
        assert_eq!(gallery.table().natural(*a), Some((2, 1)));
        assert_eq!(gallery.counts(), (1, 1, 0));
        // Disabled (--check-heights), nothing is requested.
        let off = Loading {
            remote: true,
            enabled: false,
        };
        let mut quiet = Gallery::new(crate::render::paint::image_infos(&doc), &paints, None, off);
        quiet.request(paint);
        assert_eq!(quiet.outstanding(), 0);
        assert!(matches!(quiet.state(*b), Some(ImageState::Waiting)));
        std::fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn long_sides_are_capped_keeping_proportions() {
        assert_eq!(fit_side(100, 50), (100, 50));
        assert_eq!(fit_side(4096, 1024), (2048, 512));
        assert_eq!(fit_side(1000, 8192), (250, 2048));
        assert_eq!(fit_side(2048, 2048), (2048, 2048));
        assert_eq!(fit_side(100_000, 1), (2048, 1));
    }

    #[test]
    fn files_are_read_within_the_cap() {
        let dir = std::env::temp_dir().join(format!("gbk-images-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("dot.png");
        std::fs::write(&file, png()).expect("write");
        let agent = agent();
        let texture = load(&agent, &Fetch::File(file.clone())).expect("loaded");
        assert_eq!(texture.width(), 2);
        assert!(matches!(
            load(&agent, &Fetch::File(dir.clone())),
            Err(ImageError::NotFile(path)) if path == dir
        ));
        let missing = dir.join("missing.png");
        assert!(matches!(
            load(&agent, &Fetch::File(missing)),
            Err(ImageError::Read(_, std::io::ErrorKind::NotFound))
        ));
        // Through the threads: a request comes back from `ready`.
        let mut loader = Loader::default();
        let image = crate::render::image::tests::image_ids("![a](dot.png)")[0];
        loader.request(image, Fetch::File(file));
        assert_eq!(loader.outstanding(), 1);
        let mut ready = Vec::new();
        for _ in 0..500 {
            ready.extend(loader.ready());
            if !ready.is_empty() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(ready.len(), 1);
        assert!(
            ready
                .first()
                .is_some_and(|loaded| loaded.image == image && loaded.result.is_ok())
        );
        assert_eq!(loader.outstanding(), 0);
        std::fs::remove_dir_all(&dir).expect("clean up");
    }
}
