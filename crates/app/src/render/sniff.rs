//! What an image file is, and how big it says it is, from its first bytes (docs/plan.md §4.6).
//! Pure, so it is unit-tested.
//!
//! Decoders are only ever given images whose format and size were read here first: the size
//! decides whether to decode at all (a few bytes can declare billions of pixels), and the
//! format picks the one decoder that may read it. Anything not recognised here is not shown.
//! gdk-pixbuf could not be asked: on Ubuntu 26.04 it hands most formats to glycin, out of
//! process, and glycin waits forever if told to stop (seen in M4), while the formats it keeps
//! in process (WebP, HEIF) decode in C with no size check of ours.

/// An image's format, as gdk-pixbuf names its loader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    Png,
    Jpeg,
    Gif,
    Webp,
    Bmp,
    Svg,
}

impl Format {
    /// The gdk-pixbuf loader's name.
    pub(crate) fn loader(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Gif => "gif",
            Self::Webp => "webp",
            Self::Bmp => "bmp",
            Self::Svg => "svg",
        }
    }
}

/// What the first bytes say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sniffed {
    /// A raster image declaring this many pixels across and down.
    Raster(Format, i64, i64),
    /// An SVG: drawn at whatever size is asked, so never too big to decode.
    Vector,
    Unknown,
}

/// The format and declared size of an image's bytes.
pub(crate) fn sniff(bytes: &[u8]) -> Sniffed {
    png(bytes)
        .or_else(|| gif(bytes))
        .or_else(|| jpeg(bytes))
        .or_else(|| webp(bytes))
        .or_else(|| bmp(bytes))
        .map_or_else(
            || {
                if svg(bytes) {
                    Sniffed::Vector
                } else {
                    Sniffed::Unknown
                }
            },
            |(format, width, height)| Sniffed::Raster(format, width, height),
        )
}

fn be16(bytes: &[u8], at: usize) -> Option<i64> {
    let pair = bytes.get(at..at + 2)?;
    Some(i64::from(u16::from_be_bytes([
        *pair.first()?,
        *pair.get(1)?,
    ])))
}

fn le16(bytes: &[u8], at: usize) -> Option<i64> {
    let pair = bytes.get(at..at + 2)?;
    Some(i64::from(u16::from_le_bytes([
        *pair.first()?,
        *pair.get(1)?,
    ])))
}

fn be32(bytes: &[u8], at: usize) -> Option<i64> {
    let four: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
    Some(i64::from(u32::from_be_bytes(four)))
}

fn le24(bytes: &[u8], at: usize) -> Option<i64> {
    let three = bytes.get(at..at + 3)?;
    Some(
        i64::from(*three.first()?)
            | i64::from(*three.get(1)?) << 8
            | i64::from(*three.get(2)?) << 16,
    )
}

fn le32_signed(bytes: &[u8], at: usize) -> Option<i64> {
    let four: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
    Some(i64::from(i32::from_le_bytes(four)))
}

/// PNG: the signature, then IHDR's width and height.
fn png(bytes: &[u8]) -> Option<(Format, i64, i64)> {
    (bytes.get(..8)? == b"\x89PNG\r\n\x1a\n" && bytes.get(12..16)? == b"IHDR")
        .then(|| Some((Format::Png, be32(bytes, 16)?, be32(bytes, 20)?)))?
}

/// GIF: the signature, then the logical screen's width and height.
fn gif(bytes: &[u8]) -> Option<(Format, i64, i64)> {
    let signature = bytes.get(..6)?;
    (signature == b"GIF87a" || signature == b"GIF89a")
        .then(|| Some((Format::Gif, le16(bytes, 6)?, le16(bytes, 8)?)))?
}

/// JPEG: segments from the start-of-image marker to the first start-of-frame, which gives
/// the height and width.
fn jpeg(bytes: &[u8]) -> Option<(Format, i64, i64)> {
    if bytes.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut at = 2;
    loop {
        // Markers may be padded with extra 0xFF bytes.
        while *bytes.get(at)? == 0xFF && *bytes.get(at + 1)? == 0xFF {
            at += 1;
        }
        if *bytes.get(at)? != 0xFF {
            return None;
        }
        let marker = *bytes.get(at + 1)?;
        // Markers with no length: TEM and RST0-7.
        if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            at += 2;
            continue;
        }
        let length = usize::try_from(be16(bytes, at + 2)?).ok()?;
        // SOF0-SOF15, except DHT (C4), JPG (C8) and DAC (CC).
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            return Some((Format::Jpeg, be16(bytes, at + 7)?, be16(bytes, at + 5)?));
        }
        if length < 2 {
            return None;
        }
        at += 2 + length;
    }
}

/// WebP: a RIFF container whose first chunk is lossy (VP8), lossless (VP8L) or extended (VP8X).
fn webp(bytes: &[u8]) -> Option<(Format, i64, i64)> {
    if bytes.get(..4)? != b"RIFF" || bytes.get(8..12)? != b"WEBP" {
        return None;
    }
    let (width, height) = match bytes.get(12..16)? {
        b"VP8 " => (le16(bytes, 26)? & 0x3FFF, le16(bytes, 28)? & 0x3FFF),
        b"VP8L" => {
            // After the 0x2F signature: 14 bits of width - 1, then 14 of height - 1.
            let bits = le32_signed(bytes, 21)? & 0xFFFF_FFFF;
            ((bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1)
        }
        b"VP8X" => (le24(bytes, 24)? + 1, le24(bytes, 27)? + 1),
        _ => return None,
    };
    Some((Format::Webp, width, height))
}

/// BMP: the "BM" signature, then the info header's width and (possibly negative) height.
fn bmp(bytes: &[u8]) -> Option<(Format, i64, i64)> {
    (bytes.get(..2)? == b"BM").then(|| {
        Some((
            Format::Bmp,
            le32_signed(bytes, 18)?.abs(),
            le32_signed(bytes, 22)?.abs(),
        ))
    })?
}

/// SVG: XML whose start has an `<svg` element (GitHub's badges are SVG).
fn svg(bytes: &[u8]) -> bool {
    let start = bytes.get(..bytes.len().min(4096)).unwrap_or_default();
    let text = String::from_utf8_lossy(start);
    let trimmed = text.trim_start_matches('\u{FEFF}').trim_start();
    trimmed.starts_with('<') && text.contains("<svg")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_gif_and_bmp_sizes() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend(20_000_u32.to_be_bytes());
        png.extend(3_u32.to_be_bytes());
        assert_eq!(sniff(&png), Sniffed::Raster(Format::Png, 20_000, 3));
        assert_eq!(sniff(png.get(..20).expect("short")), Sniffed::Unknown);
        let gif = b"GIF89a\x40\x01\xF0\x00rest";
        assert_eq!(sniff(gif), Sniffed::Raster(Format::Gif, 320, 240));
        let mut bmp = b"BM".to_vec();
        bmp.resize(18, 0);
        bmp.extend(640_i32.to_le_bytes());
        bmp.extend((-480_i32).to_le_bytes());
        assert_eq!(sniff(&bmp), Sniffed::Raster(Format::Bmp, 640, 480));
    }

    #[test]
    fn jpeg_size_comes_from_the_first_frame_header() {
        // SOI, an APP0 segment of 16 bytes, padding, then SOF0: height 480, width 640.
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        jpeg.extend([0; 14]);
        jpeg.extend([0xFF, 0xFF, 0xC0, 0x00, 0x11, 0x08, 0x01, 0xE0, 0x02, 0x80]);
        assert_eq!(sniff(&jpeg), Sniffed::Raster(Format::Jpeg, 640, 480));
        // A DHT segment (C4) is not a frame header.
        let mut dht = vec![0xFF, 0xD8, 0xFF, 0xC4, 0x00, 0x04, 0, 0];
        dht.extend([0xFF, 0xC2, 0x00, 0x11, 0x08, 0x00, 0x10, 0x00, 0x20]);
        assert_eq!(sniff(&dht), Sniffed::Raster(Format::Jpeg, 32, 16));
        // Truncated, or a zero length that would loop: not an image.
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Sniffed::Unknown);
        assert_eq!(
            sniff(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x00, 0xFF]),
            Sniffed::Unknown
        );
    }

    #[test]
    fn webp_sizes_in_all_three_forms() {
        let riff = |chunk: &[u8], body: &[u8]| {
            let mut bytes = b"RIFF\0\0\0\0WEBP".to_vec();
            bytes.extend(chunk);
            bytes.extend(body);
            bytes
        };
        // VP8: 10 bytes into the chunk data, 14-bit width and height.
        let mut lossy = vec![0; 10];
        lossy.extend(400_u16.to_le_bytes());
        lossy.extend(300_u16.to_le_bytes());
        assert_eq!(
            sniff(&riff(b"VP8 ", &lossy)),
            Sniffed::Raster(Format::Webp, 400, 300)
        );
        // VP8L: size, signature, then (width - 1) | (height - 1) << 14: 99 and 49.
        let packed: u32 = 0x63 | (0x31 << 14);
        let mut lossless = vec![0, 0, 0, 0, 0x2F];
        lossless.extend(packed.to_le_bytes());
        assert_eq!(
            sniff(&riff(b"VP8L", &lossless)),
            Sniffed::Raster(Format::Webp, 100, 50)
        );
        // VP8X: canvas width - 1 and height - 1, 24 bits each.
        let mut extended = vec![0; 8];
        extended.extend([0x0F, 0x27, 0x00, 0x0F, 0x27, 0x00]);
        assert_eq!(
            sniff(&riff(b"VP8X", &extended)),
            Sniffed::Raster(Format::Webp, 10_000, 10_000)
        );
        assert_eq!(sniff(&riff(b"ALPH", &extended)), Sniffed::Unknown);
    }

    #[test]
    fn svg_is_vector_and_anything_else_unknown() {
        assert_eq!(
            sniff(b"<svg xmlns='http://www.w3.org/2000/svg'/>"),
            Sniffed::Vector
        );
        assert_eq!(
            sniff(b"\xEF\xBB\xBF  <?xml version='1.0'?>\n<svg/>"),
            Sniffed::Vector
        );
        assert_eq!(
            sniff(b"<html><body>no image</body></html>"),
            Sniffed::Unknown
        );
        assert_eq!(sniff(b"not an image"), Sniffed::Unknown);
        assert_eq!(sniff(b""), Sniffed::Unknown);
        // TIFF and HEIF are not shown.
        assert_eq!(sniff(b"II*\0\x08\0\0\0"), Sniffed::Unknown);
        assert_eq!(sniff(b"\0\0\0\x18ftypheic"), Sniffed::Unknown);
        assert_eq!(Format::Svg.loader(), "svg");
    }
}
