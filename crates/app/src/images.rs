//! Loading images off the main thread (docs/plan.md §4.6).
//!
//! The view marks an image wanted once its placeholder has been drawn while the view is still
//! (not mid-fling), and sends what it wants after the frame, so nothing is read or fetched
//! before the first frame. A few threads, started on the first request, read local files or
//! fetch `http(s)` URLs with a timeout and a size cap, decode with gdk-pixbuf (any format its
//! loaders know: PNG, JPEG, GIF's first frame, SVG through glycin...), and hand back a
//! `gdk::Texture`, which GDK allows to cross threads. The view collects arrivals between
//! frames. Images with the same source load once.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use gbk_markdown_core::units::ImageId;
use gtk::gdk;
use gtk::gdk_pixbuf;
use gtk::glib;
use gtk::prelude::*;

use crate::links;
use crate::render::image::{
    ImageInfo, ImageSource, ImageTable, decode_size, premultiplied_bgra, too_many_pixels,
};
use crate::render::paint::LeafPaint;
use crate::render::sniff::{Format, Sniffed, sniff};

/// Threads loading at once: enough for a README's row of badges, few enough to be polite.
const THREADS: usize = 4;
/// Most bytes read or fetched for one image.
const MAX_BYTES: u64 = 10 * 1024 * 1024;
/// A remote image that takes longer than this, redirects included, fails.
const TIMEOUT: Duration = Duration::from_secs(20);
const MAX_REDIRECTS: u32 = 5;
/// An image still loading after this is shown as failed, whatever its thread is doing (a
/// decoder that never returns would otherwise leave it "loading" for good).
const DEADLINE: Duration = Duration::from_secs(30);
/// Decoded images kept beyond this many bytes are forgotten when far from the view.
pub(crate) const TEXTURE_BUDGET: usize = 256 * 1024 * 1024;

/// What the view has of an image.
#[derive(Debug, Clone)]
pub(crate) enum ImageState {
    /// Not wanted yet: its placeholder has not been drawn.
    Waiting,
    /// Wanted; sent to the threads after this frame.
    Wanted,
    Loading {
        since: Instant,
    },
    Ready(gdk::Texture),
    Failed(ImageError),
    /// A remote image, with `--offline`.
    Offline,
    /// Nowhere to load it from: an unsupported source, or no document file to start from.
    NoSource,
}

/// Images shown, failed, and still loading, for the probe.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ImageCounts {
    pub(crate) loaded: usize,
    pub(crate) failed: usize,
    pub(crate) loading: usize,
}

/// The view's images: the shared table of sizes, what each image is now, and the loads in
/// flight.
#[derive(Debug)]
pub(crate) struct Gallery {
    table: Arc<ImageTable>,
    states: Vec<ImageState>,
    /// Each image's id, by index (core makes the ids).
    ids: Vec<ImageId>,
    fetcher: Fetcher,
    /// The paints each image is in, to draw again when it arrives.
    paints: HashMap<ImageId, Vec<usize>>,
    /// For each source being loaded, the images waiting on it.
    sharing: HashMap<Fetch, Vec<ImageId>>,
    /// The document's file, which relative paths start from.
    file: Option<PathBuf>,
    /// Fetch no remote images (`--offline`).
    offline: bool,
    /// Load nothing (while `--check-heights` compares layouts without `--wait-images`).
    enabled: bool,
}

impl Gallery {
    /// The images of `paints`, described by `infos` (in id order).
    pub(crate) fn new(
        infos: Vec<ImageInfo>,
        paints: &[LeafPaint],
        file: Option<PathBuf>,
        offline: bool,
        enabled: bool,
    ) -> Self {
        let mut by_image: HashMap<ImageId, Vec<usize>> = HashMap::new();
        let mut ids: Vec<Option<ImageId>> = vec![None; infos.len()];
        for (index, paint) in paints.iter().enumerate() {
            for image in &paint.images {
                by_image.entry(*image).or_default().push(index);
                if let Some(slot) = ids.get_mut(image.index()) {
                    *slot = Some(*image);
                }
            }
        }
        let count = infos.len();
        Self {
            table: Arc::new(ImageTable::new(infos)),
            states: vec![ImageState::Waiting; count],
            ids: ids.into_iter().flatten().collect(),
            fetcher: Fetcher::default(),
            paints: by_image,
            sharing: HashMap::new(),
            file,
            offline,
            enabled,
        }
    }

    pub(crate) fn table(&self) -> &Arc<ImageTable> {
        &self.table
    }

    pub(crate) fn state(&self, image: ImageId) -> Option<&ImageState> {
        self.states.get(image.index())
    }

    /// The paints showing an image.
    pub(crate) fn paints_of(&self, image: ImageId) -> &[usize] {
        self.paints.get(&image).map_or(&[], Vec::as_slice)
    }

    /// Marks a drawn paint's waiting images as wanted (`dispatch` sends them); returns
    /// whether any was.
    pub(crate) fn want(&mut self, paint: &LeafPaint) -> bool {
        if !self.enabled {
            return false;
        }
        let mut wanted = false;
        for &image in &paint.images {
            if !matches!(self.state(image), Some(ImageState::Waiting)) {
                continue;
            }
            let state = match self.source(image) {
                Ok(_) => {
                    wanted = true;
                    ImageState::Wanted
                }
                Err(state) => state,
            };
            self.set(image, state);
        }
        wanted
    }

    /// Sends the wanted images to the threads: each source once.
    pub(crate) fn dispatch(&mut self, now: Instant) {
        let wanted: Vec<ImageId> = self
            .ids
            .iter()
            .copied()
            .filter(|&image| matches!(self.state(image), Some(ImageState::Wanted)))
            .collect();
        for image in wanted {
            let Ok(fetch) = self.source(image) else {
                continue;
            };
            self.set(image, ImageState::Loading { since: now });
            if let Some(waiting) = self.sharing.get_mut(&fetch) {
                waiting.push(image);
            } else {
                self.sharing.insert(fetch.clone(), vec![image]);
                self.fetcher.request(fetch);
            }
        }
    }

    /// Where an image loads from, or the state it takes if it does not load.
    fn source(&self, image: ImageId) -> Result<Fetch, ImageState> {
        let source = self
            .table
            .info(image)
            .and_then(|info| info.source.as_ref())
            .ok_or(ImageState::NoSource)?;
        match source {
            ImageSource::Url(_) if self.offline => Err(ImageState::Offline),
            ImageSource::Url(url) => Ok(Fetch::Url(url.clone())),
            ImageSource::Path(path) => {
                let file = self.file.clone().ok_or(ImageState::NoSource)?;
                Ok(Fetch::File {
                    document: file,
                    path: path.clone(),
                })
            }
        }
    }

    /// Takes the loads finished since last asked, and fails those past the deadline; returns
    /// the images whose state changed.
    pub(crate) fn collect(&mut self, now: Instant) -> Vec<ImageId> {
        let mut changed = Vec::new();
        for Arrival { fetch, result } in self.fetcher.arrivals() {
            for image in self.sharing.remove(&fetch).unwrap_or_default() {
                // Too late: already shown as failed.
                if !matches!(self.state(image), Some(ImageState::Loading { .. })) {
                    continue;
                }
                let state = match &result {
                    Ok(Decoded { texture, natural }) => {
                        self.table.set_natural(image, *natural);
                        ImageState::Ready(texture.clone())
                    }
                    Err(error) => ImageState::Failed(error.clone()),
                };
                self.set(image, state);
                changed.push(image);
            }
        }
        let late: Vec<ImageId> = self
            .ids
            .iter()
            .copied()
            .filter(|&image| {
                matches!(self.state(image), Some(ImageState::Loading { since })
                    if now.duration_since(*since) > DEADLINE)
            })
            .collect();
        for image in late {
            self.set(image, ImageState::Failed(ImageError::TimedOut));
            changed.push(image);
        }
        changed
    }

    fn set(&mut self, image: ImageId, state: ImageState) {
        if let Some(slot) = self.states.get_mut(image.index()) {
            *slot = state;
        }
    }

    /// Above `budget` bytes of decoded images (`TEXTURE_BUDGET`), forgets those not `kept`
    /// (images of paints near the view): they go back to waiting, keeping their size (so
    /// nothing moves), and load again when next drawn.
    pub(crate) fn trim(&mut self, budget: usize, kept: &dyn Fn(ImageId) -> bool) {
        let bytes = |state: &ImageState| match state {
            ImageState::Ready(texture) => {
                usize::try_from(i64::from(texture.width()) * i64::from(texture.height()) * 4)
                    .unwrap_or(usize::MAX)
            }
            ImageState::Waiting
            | ImageState::Wanted
            | ImageState::Loading { .. }
            | ImageState::Failed(_)
            | ImageState::Offline
            | ImageState::NoSource => 0,
        };
        let total: usize = self.states.iter().map(bytes).sum();
        if total <= budget {
            return;
        }
        let far: Vec<ImageId> = self
            .ids
            .iter()
            .copied()
            .filter(|&image| {
                !kept(image) && matches!(self.state(image), Some(ImageState::Ready(_)))
            })
            .collect();
        for image in far {
            self.set(image, ImageState::Waiting);
        }
    }

    /// Whether anything is wanted or loading.
    pub(crate) fn busy(&self) -> bool {
        self.states
            .iter()
            .any(|state| matches!(state, ImageState::Wanted | ImageState::Loading { .. }))
    }

    pub(crate) fn counts(&self) -> ImageCounts {
        let mut counts = ImageCounts::default();
        for state in &self.states {
            match state {
                ImageState::Ready(_) => counts.loaded += 1,
                ImageState::Failed(_) => counts.failed += 1,
                ImageState::Wanted | ImageState::Loading { .. } => counts.loading += 1,
                ImageState::Waiting | ImageState::Offline | ImageState::NoSource => {}
            }
        }
        counts
    }
}

/// Where to load an image from. A relative path is resolved on the loading thread (finding
/// the repository's root touches the disk).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Fetch {
    Url(String),
    File { document: PathBuf, path: String },
}

/// Why an image could not be shown.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ImageError {
    #[error("cannot read {0}: {1}")]
    Read(PathBuf, std::io::ErrorKind),
    #[error("{0} is not a file")]
    NotFile(PathBuf),
    #[error("larger than {MAX_BYTES} bytes")]
    TooLarge,
    #[error("the server answered {0}")]
    Status(u16),
    #[error("took too long")]
    TimedOut,
    #[error("its server was not found")]
    HostNotFound,
    #[error("a network error: {0}")]
    Network(std::io::ErrorKind),
    #[error("cannot fetch it")]
    FetchFailed,
    #[error("not an image this viewer can show")]
    NotAnImage,
    #[error("the image data is damaged")]
    Corrupt,
    #[error("declares {0} × {1} pixels, more than shown")]
    TooManyPixels(i64, i64),
    #[error("the image loader stopped")]
    LoaderGone,
}

impl From<ureq::Error> for ImageError {
    fn from(error: ureq::Error) -> Self {
        #[expect(
            clippy::wildcard_enum_match_arm,
            reason = "ureq's error is not ours to list; any other failure is just a failed fetch"
        )]
        match error {
            ureq::Error::StatusCode(status) => Self::Status(status),
            ureq::Error::Timeout(_) => Self::TimedOut,
            ureq::Error::HostNotFound => Self::HostNotFound,
            ureq::Error::Io(error) => Self::Network(error.kind()),
            ureq::Error::BodyExceedsLimit(_) => Self::TooLarge,
            _ => Self::FetchFailed,
        }
    }
}

impl From<glib::Error> for ImageError {
    fn from(error: glib::Error) -> Self {
        // gdk-pixbuf's error enum is non-exhaustive: anything else is damaged data.
        match error.kind::<gdk_pixbuf::PixbufError>() {
            Some(
                gdk_pixbuf::PixbufError::UnknownType
                | gdk_pixbuf::PixbufError::UnsupportedOperation,
            ) => Self::NotAnImage,
            Some(gdk_pixbuf::PixbufError::InsufficientMemory) => Self::TooLarge,
            _ => Self::Corrupt,
        }
    }
}

/// A finished load.
#[derive(Debug)]
struct Arrival {
    fetch: Fetch,
    result: Result<Decoded, ImageError>,
}

/// The loading threads, started on the first request.
#[derive(Debug)]
struct Fetcher {
    requests: Option<Sender<Fetch>>,
    arrivals: Receiver<Arrival>,
    arrivals_sender: Sender<Arrival>,
}

impl Default for Fetcher {
    fn default() -> Self {
        let (arrivals_sender, arrivals) = mpsc::channel();
        Self {
            requests: None,
            arrivals,
            arrivals_sender,
        }
    }
}

impl Fetcher {
    fn request(&mut self, fetch: Fetch) {
        let requests = self
            .requests
            .get_or_insert_with(|| start(&self.arrivals_sender));
        if let Err(mpsc::SendError(fetch)) = requests.send(fetch) {
            // Every thread has stopped: say so for this image, through the usual path.
            drop(self.arrivals_sender.send(Arrival {
                fetch,
                result: Err(ImageError::LoaderGone),
            }));
        }
    }

    fn arrivals(&self) -> Vec<Arrival> {
        self.arrivals.try_iter().collect()
    }
}

/// Starts the threads; returns where to send requests.
fn start(arrivals: &Sender<Arrival>) -> Sender<Fetch> {
    let (requests, queue) = mpsc::channel::<Fetch>();
    let queue = Arc::new(Mutex::new(queue));
    let agent = Arc::new(agent());
    for _ in 0..THREADS {
        let queue = Arc::clone(&queue);
        let arrivals = arrivals.clone();
        let agent = Arc::clone(&agent);
        thread::spawn(move || {
            loop {
                // One thread waits on the queue at a time; the lock is released before loading.
                let next = queue
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .recv();
                let Ok(fetch) = next else {
                    return;
                };
                let result = fetch_and_decode(&agent, &fetch);
                if arrivals.send(Arrival { fetch, result }).is_err() {
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

fn fetch_and_decode(agent: &ureq::Agent, fetch: &Fetch) -> Result<Decoded, ImageError> {
    let bytes = match fetch {
        Fetch::File { document, path } => {
            let is_repository = |folder: &Path| folder.join(".git").exists();
            read(&links::resolve_path(document, path, &is_repository))?
        }
        Fetch::Url(url) => agent
            .get(url)
            .call()?
            .body_mut()
            .with_config()
            .limit(MAX_BYTES)
            .read_to_vec()?,
    };
    decode(&bytes)
}

fn read(path: &Path) -> Result<Vec<u8>, ImageError> {
    let fail = |error: std::io::Error| ImageError::Read(path.to_path_buf(), error.kind());
    let file = std::fs::File::open(path).map_err(fail)?;
    // Checked on the open file, so what is read is what was checked (not a FIFO or device
    // swapped in after).
    let metadata = file.metadata().map_err(fail)?;
    if !metadata.is_file() {
        return Err(ImageError::NotFile(path.to_path_buf()));
    }
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut std::io::Read::take(file, MAX_BYTES + 1), &mut bytes)
        .map_err(fail)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_BYTES {
        return Err(ImageError::TooLarge);
    }
    Ok(bytes)
}

/// A decoded image: its texture, no larger than `render::image::MAX_SIDE`, and its own size.
#[derive(Debug, Clone)]
struct Decoded {
    texture: gdk::Texture,
    natural: (i64, i64),
}

/// Decodes an image whose format and size `sniff` recognised: too many pixels, or a format it
/// does not know, and no decoder sees the bytes. The decoder is the one for that format only.
fn decode(bytes: &[u8]) -> Result<Decoded, ImageError> {
    let (format, declared) = match sniff(bytes) {
        Sniffed::Raster(_, width, height) if width <= 0 || height <= 0 => {
            return Err(ImageError::Corrupt);
        }
        Sniffed::Raster(_, width, height) if too_many_pixels(width, height) => {
            return Err(ImageError::TooManyPixels(width, height));
        }
        Sniffed::Raster(format, width, height) => (format, Some((width, height))),
        Sniffed::Vector => (Format::Svg, None),
        Sniffed::Unknown => return Err(ImageError::NotAnImage),
    };
    let loader = gdk_pixbuf::PixbufLoader::with_type(format.loader())
        .map_err(|_no_loader| ImageError::NotAnImage)?;
    let size = Rc::new(Cell::new(declared));
    let seen = Rc::clone(&size);
    // Called once the decoder has read the size, before it allocates pixels.
    loader.connect_size_prepared(move |loader, width, height| {
        let (width, height) = (i64::from(width), i64::from(height));
        seen.set(seen.get().or(Some((width, height))));
        if let Some((width, height)) = decode_size(width, height) {
            loader.set_size(
                i32::try_from(width).unwrap_or(1),
                i32::try_from(height).unwrap_or(1),
            );
        }
    });
    loader.write(bytes)?;
    loader.close()?;
    let pixbuf = loader.pixbuf().ok_or(ImageError::NotAnImage)?;
    let texture = texture(&pixbuf);
    let natural = size
        .get()
        .unwrap_or((i64::from(texture.width()), i64::from(texture.height())));
    Ok(Decoded { texture, natural })
}

/// A pixbuf's pixels as a texture, in Cairo's own pixel format.
fn texture(pixbuf: &gdk_pixbuf::Pixbuf) -> gdk::Texture {
    let width = usize::try_from(pixbuf.width()).unwrap_or(0);
    let height = usize::try_from(pixbuf.height()).unwrap_or(0);
    let stride = usize::try_from(pixbuf.rowstride()).unwrap_or(0);
    let pixels = premultiplied_bgra(
        &pixbuf.read_pixel_bytes(),
        (width, height),
        stride,
        pixbuf.has_alpha(),
    );
    gdk::MemoryTexture::new(
        pixbuf.width(),
        pixbuf.height(),
        gdk::MemoryFormat::B8g8r8a8Premultiplied,
        &glib::Bytes::from_owned(pixels),
        width * 4,
    )
    .upcast()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2 × 1 PNG, red then blue.
    fn png() -> Vec<u8> {
        sized_png(2, 1)
    }

    /// A PNG of `width` × `height`, red.
    fn sized_png(width: i32, height: i32) -> Vec<u8> {
        let pixbuf = gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, false, 8, width, height)
            .expect("a pixbuf");
        pixbuf.fill(0xFF00_00FF);
        if width > 1 {
            pixbuf.put_pixel(1, 0, 0, 0, 255, 255);
        }
        pixbuf.save_to_bufferv("png", &[]).expect("encoded")
    }

    /// A PNG whose header declares `width` × `height` (checksum fixed up).
    fn declaring(width: u32, height: u32) -> Vec<u8> {
        let mut png = png();
        // IHDR's width and height are bytes 16..24, its checksum 29..33 (over 12..29).
        png.splice(16..24, [width.to_be_bytes(), height.to_be_bytes()].concat());
        let crc = crc32(png.get(12..29).expect("IHDR"));
        png.splice(29..33, crc.to_be_bytes());
        png
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
    fn images_decode_to_their_size() {
        let decoded = decode(&png()).expect("decoded");
        assert_eq!((decoded.texture.width(), decoded.texture.height()), (2, 1));
        assert_eq!(decoded.natural, (2, 1));
    }

    #[test]
    fn rubbish_is_not_an_image() {
        assert_eq!(
            decode(b"not an image").map(|_| ()),
            Err(ImageError::NotAnImage)
        );
        assert_eq!(decode(b"").map(|_| ()), Err(ImageError::NotAnImage));
    }

    #[test]
    fn svgs_are_drawn_no_larger_than_the_cap() {
        // Needs gdk-pixbuf's SVG loader (librsvg2-common in the Dockerfile, glycin on Ubuntu
        // 26.04): an SVG declaring 100 000 × 50 000 pixels is drawn at 2048 × 1024.
        let huge =
            br#"<svg xmlns="http://www.w3.org/2000/svg" width="100000" height="50000"></svg>"#;
        let decoded = decode(huge).expect("an SVG");
        assert_eq!(
            (decoded.texture.width(), decoded.texture.height()),
            (2048, 1024)
        );
        assert_eq!(decoded.natural, (100_000, 50_000));
    }

    #[test]
    fn a_small_png_declaring_too_many_pixels_is_refused() {
        // A PNG whose header says 20 000 × 20 000 (400 megapixels), in a few bytes: refused
        // before any decoder sees it.
        let png = declaring(20_000, 20_000);
        assert_eq!(
            decode(&png).map(|_| ()),
            Err(ImageError::TooManyPixels(20_000, 20_000))
        );
    }

    #[test]
    fn a_png_declaring_no_pixels_is_damaged() {
        assert_eq!(
            decode(&declaring(0, 5)).map(|_| ()),
            Err(ImageError::Corrupt)
        );
        assert_eq!(
            decode(&declaring(5, 0)).map(|_| ()),
            Err(ImageError::Corrupt)
        );
    }

    #[test]
    fn foreign_errors_map_to_ours_by_kind() {
        assert_eq!(
            ImageError::from(ureq::Error::StatusCode(404)),
            ImageError::Status(404)
        );
        assert_eq!(
            ImageError::from(ureq::Error::Timeout(ureq::Timeout::Global)),
            ImageError::TimedOut
        );
        assert_eq!(
            ImageError::from(ureq::Error::HostNotFound),
            ImageError::HostNotFound
        );
        assert_eq!(
            ImageError::from(ureq::Error::Io(std::io::Error::from(
                std::io::ErrorKind::ConnectionReset
            ))),
            ImageError::Network(std::io::ErrorKind::ConnectionReset)
        );
        assert_eq!(
            ImageError::from(ureq::Error::BodyExceedsLimit(9)),
            ImageError::TooLarge
        );
        assert_eq!(
            ImageError::from(ureq::Error::ConnectionFailed),
            ImageError::FetchFailed
        );
        let pixbuf = |kind| glib::Error::new(kind, "message text is never read");
        assert_eq!(
            ImageError::from(pixbuf(gdk_pixbuf::PixbufError::UnknownType)),
            ImageError::NotAnImage
        );
        assert_eq!(
            ImageError::from(pixbuf(gdk_pixbuf::PixbufError::UnsupportedOperation)),
            ImageError::NotAnImage
        );
        assert_eq!(
            ImageError::from(pixbuf(gdk_pixbuf::PixbufError::InsufficientMemory)),
            ImageError::TooLarge
        );
        assert_eq!(
            ImageError::from(pixbuf(gdk_pixbuf::PixbufError::CorruptImage)),
            ImageError::Corrupt
        );
    }

    #[test]
    fn files_up_to_the_cap_are_read_and_no_more() {
        let dir = std::env::temp_dir().join(format!("gbk-cap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let cap = usize::try_from(MAX_BYTES).expect("small");
        assert_eq!(cap, 10 * 1024 * 1024);
        std::fs::write(dir.join("exact"), vec![0_u8; cap]).expect("write");
        std::fs::write(dir.join("over"), vec![0_u8; cap + 1]).expect("write");
        assert_eq!(read(&dir.join("exact")).map(|bytes| bytes.len()), Ok(cap));
        assert_eq!(read(&dir.join("over")), Err(ImageError::TooLarge));
        assert_eq!(TEXTURE_BUDGET, 256 * 1024 * 1024);
        std::fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn files_are_read_within_the_cap() {
        let dir = std::env::temp_dir().join(format!("gbk-images-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join("dot.png"), png()).expect("write");
        let agent = agent();
        let file = |path: &str| Fetch::File {
            document: dir.join("doc.md"),
            path: path.to_owned(),
        };
        let decoded = fetch_and_decode(&agent, &file("dot.png")).expect("loaded");
        assert_eq!(decoded.texture.width(), 2);
        assert_eq!(
            fetch_and_decode(&agent, &file(".")).map(|_| ()),
            Err(ImageError::NotFile(dir.clone()))
        );
        assert_eq!(
            fetch_and_decode(&agent, &file("missing.png")).map(|_| ()),
            Err(ImageError::Read(
                dir.join("missing.png"),
                std::io::ErrorKind::NotFound
            ))
        );
        std::fs::remove_dir_all(&dir).expect("clean up");
    }

    /// Collects until nothing is loading, or 5 s.
    fn settle(gallery: &mut Gallery) -> Vec<ImageId> {
        let mut changed = Vec::new();
        for _ in 0..500 {
            changed.extend(gallery.collect(Instant::now()));
            if !gallery.busy() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        changed
    }

    #[test]
    fn the_gallery_loads_wanted_images_once_per_source() {
        let dir = std::env::temp_dir().join(format!("gbk-gallery-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join("dot.png"), png()).expect("write");
        let markdown =
            "![a](dot.png) ![b](https://x.org/b.png) ![c](missing.png) ![d](#x) ![e](dot.png)\n";
        let doc = gbk_markdown_core::load(markdown.as_bytes());
        let paints = crate::render::paint::paint_document(&doc);
        let infos = crate::render::paint::image_infos(&doc);
        let mut gallery = Gallery::new(infos, &paints, Some(dir.join("doc.md")), true, true);
        let paint = paints.first().expect("paragraph");
        let [first, remote, missing, fragment, again] = paint.images.as_slice() else {
            panic!("five images");
        };
        assert!(matches!(gallery.state(*first), Some(ImageState::Waiting)));
        assert!(gallery.want(paint));
        assert!(!gallery.want(paint));
        assert!(matches!(gallery.state(*first), Some(ImageState::Wanted)));
        assert!(matches!(gallery.state(*remote), Some(ImageState::Offline)));
        assert!(matches!(
            gallery.state(*fragment),
            Some(ImageState::NoSource)
        ));
        gallery.dispatch(Instant::now());
        // a and e share a source: one fetch for both.
        assert_eq!(gallery.sharing.len(), 2);
        assert_eq!(gallery.counts().loading, 3);
        let mut changed = settle(&mut gallery);
        changed.sort_unstable();
        let mut expected = vec![*first, *missing, *again];
        expected.sort_unstable();
        assert_eq!(changed, expected);
        assert!(matches!(gallery.state(*first), Some(ImageState::Ready(_))));
        assert!(matches!(gallery.state(*again), Some(ImageState::Ready(_))));
        assert!(matches!(
            gallery.state(*missing),
            Some(ImageState::Failed(ImageError::Read(..)))
        ));
        assert_eq!(gallery.table().natural(*again), Some((2, 1)));
        assert_eq!(
            gallery.counts(),
            ImageCounts {
                loaded: 2,
                failed: 1,
                loading: 0
            }
        );
        assert_eq!(gallery.paints_of(*first), &[0]);
        // Online, the remote image is wanted.
        let infos = crate::render::paint::image_infos(&doc);
        let mut online = Gallery::new(infos, &paints, None, false, true);
        online.want(paint);
        assert!(matches!(online.state(*remote), Some(ImageState::Wanted)));
        // Disabled (--check-heights), nothing is wanted.
        let infos = crate::render::paint::image_infos(&doc);
        let mut quiet = Gallery::new(infos, &paints, None, false, false);
        assert!(!quiet.want(paint));
        assert!(matches!(quiet.state(*remote), Some(ImageState::Waiting)));
        std::fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn far_images_are_forgotten_over_the_budget() {
        let dir = std::env::temp_dir().join(format!("gbk-trim-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        // 3 × 2 pixels: 24 bytes decoded.
        std::fs::write(dir.join("dot.png"), sized_png(3, 2)).expect("write");
        let doc = gbk_markdown_core::load(b"text\n\n![a](dot.png)\n\n![b](dot.png)\n");
        let paints = crate::render::paint::paint_document(&doc);
        let infos = crate::render::paint::image_infos(&doc);
        let mut gallery = Gallery::new(infos, &paints, Some(dir.join("doc.md")), true, true);
        for paint in &paints {
            gallery.want(paint);
        }
        gallery.dispatch(Instant::now());
        settle(&mut gallery);
        let near = *paints.get(1).and_then(|p| p.images.first()).expect("a");
        let far = *paints.get(2).and_then(|p| p.images.first()).expect("b");
        assert_eq!(gallery.paints_of(far), &[2]);
        // Two images of 24 bytes: within 48, nothing goes; over 47, the far one does.
        gallery.trim(48, &|image| image == near);
        assert_eq!(gallery.counts().loaded, 2);
        gallery.trim(47, &|image| image == near);
        assert!(matches!(gallery.state(near), Some(ImageState::Ready(_))));
        assert!(matches!(gallery.state(far), Some(ImageState::Waiting)));
        // Its size stays known, so its paint keeps its height.
        assert_eq!(gallery.table().natural(far), Some((3, 2)));
        std::fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn loads_past_the_deadline_fail() {
        let doc = gbk_markdown_core::load(b"![a](https://example.invalid/a.png)\n");
        let paints = crate::render::paint::paint_document(&doc);
        let mut gallery = Gallery::new(
            crate::render::paint::image_infos(&doc),
            &paints,
            None,
            false,
            true,
        );
        let paint = paints.first().expect("paragraph");
        let image = *paint.images.first().expect("an image");
        // Marked loading long ago, without sending it: the deadline fails it.
        let long_ago = Instant::now()
            .checked_sub(DEADLINE * 2)
            .expect("a past instant");
        gallery.set(image, ImageState::Loading { since: long_ago });
        assert_eq!(gallery.collect(Instant::now()), vec![image]);
        assert!(matches!(
            gallery.state(image),
            Some(ImageState::Failed(ImageError::TimedOut))
        ));
        assert!(!gallery.busy());
    }
}
