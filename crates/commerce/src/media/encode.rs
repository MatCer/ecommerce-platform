//! Image verification and re-encoding (spec §14: MIME sniffing, size limits, re-encode that
//! strips EXIF). Pure and blocking: callers run it on a blocking thread.
//!
//! Crates: `image` (pure-Rust decoders with allocation limits, JPEG/PNG encoders), `ravif`
//! (AVIF via rav1e, pure Rust), `webp` (libwebp: `image` only encodes lossless WebP), `infer`
//! (magic-number sniffing).

use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ImageDecoder, ImageEncoder, ImageReader, Limits};

/// Largest accepted upload (spec §8.1).
pub const MAX_BYTES: u64 = 20 * 1024 * 1024;
/// Largest accepted side and pixel count: bounds decode memory (decompression bombs).
pub const MAX_SIDE: u32 = 12_000;
pub const MAX_PIXELS: u64 = 50_000_000;
/// Accepted upload types (sniffed from the bytes, never trusted from the client).
pub const ACCEPTED: &[&str] = &["image/jpeg", "image/png", "image/webp", "image/gif"];
/// Responsive widths; an image is never upscaled. 480 and 720 exist for phones: a
/// full-width product image on a 412 px viewport at DPR 1.75 needs 665 px, and without 720 the
/// browser takes the 960 variant, about 3x heavier (measured with `make perf` in WP6: PDP LCP
/// 1.8 s instead of 1.2 s).
pub const WIDTHS: &[u32] = &[160, 320, 480, 640, 720, 960, 1280, 1920];

/// rav1e speed 0-10 (10 fastest) and quality; speed 8 keeps a 1920 px encode around a second.
const AVIF_SPEED: u8 = 8;
// Balance photo detail against mobile transfer size; see docs/acceptance/wp25-perf.md.
const AVIF_QUALITY: f32 = 50.0;
const WEBP_QUALITY: f32 = 75.0;
const JPEG_QUALITY: u8 = 80;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejected {
    TooLarge,
    NotAnImage,
    Unsupported(String),
    TooManyPixels,
    Corrupt(String),
}

impl Rejected {
    pub fn code(&self) -> &'static str {
        match self {
            Self::TooLarge => "file_too_large",
            Self::NotAnImage | Self::Unsupported(_) => "unsupported_type",
            Self::TooManyPixels => "image_too_large",
            Self::Corrupt(_) => "corrupt_image",
        }
    }

    pub fn detail(&self) -> String {
        match self {
            Self::TooLarge => format!("uploads are limited to {} MB", MAX_BYTES / 1024 / 1024),
            Self::NotAnImage => "the file is not a supported image".into(),
            Self::Unsupported(mime) => format!("{mime} is not accepted (JPEG, PNG, WebP, GIF)"),
            Self::TooManyPixels => format!(
                "images are limited to {MAX_SIDE} px per side and {} MP",
                MAX_PIXELS / 1_000_000
            ),
            Self::Corrupt(e) => format!("the image cannot be read: {e}"),
        }
    }
}

/// What verification learned about an upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    pub mime: &'static str,
    pub width: u32,
    pub height: u32,
}

/// Sniffs the type and reads the dimensions from the header (no full decode).
pub fn verify(bytes: &[u8]) -> Result<Verified, Rejected> {
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_BYTES {
        return Err(Rejected::TooLarge);
    }
    let kind = infer::get(bytes).ok_or(Rejected::NotAnImage)?;
    let mime = ACCEPTED
        .iter()
        .find(|m| **m == kind.mime_type())
        .ok_or_else(|| Rejected::Unsupported(kind.mime_type().to_owned()))?;
    let (width, height) = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| Rejected::Corrupt(e.to_string()))?
        .into_dimensions()
        .map_err(|e| Rejected::Corrupt(e.to_string()))?;
    if width == 0
        || height == 0
        || width > MAX_SIDE
        || height > MAX_SIDE
        || u64::from(width) * u64::from(height) > MAX_PIXELS
    {
        return Err(Rejected::TooManyPixels);
    }
    Ok(Verified {
        mime,
        width,
        height,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Avif,
    Webp,
    Jpeg,
    Png,
}

impl Format {
    pub fn ext(self) -> &'static str {
        match self {
            Self::Avif => "avif",
            Self::Webp => "webp",
            Self::Jpeg => "jpg",
            Self::Png => "png",
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            Self::Avif => "image/avif",
            Self::Webp => "image/webp",
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Jpeg => "jpeg",
            other => other.ext(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Encoded {
    pub width: u32,
    pub height: u32,
    pub format: Format,
    pub bytes: Vec<u8>,
}

/// Decodes (with limits), applies the EXIF orientation, and re-encodes every width as AVIF,
/// WebP and a JPEG (opaque) or PNG (transparent) fallback. Output carries pixels only: EXIF,
/// XMP, ICC and comments of the original are gone.
pub fn render(original: &[u8]) -> Result<Vec<Encoded>, Rejected> {
    verify(original)?;
    let mut reader = ImageReader::new(Cursor::new(original))
        .with_guessed_format()
        .map_err(|e| Rejected::Corrupt(e.to_string()))?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(512 * 1024 * 1024);
    reader.limits(limits);
    let mut decoder = reader
        .into_decoder()
        .map_err(|e| Rejected::Corrupt(e.to_string()))?;
    let orientation = decoder
        .orientation()
        .map_err(|e| Rejected::Corrupt(e.to_string()))?;
    let mut img =
        DynamicImage::from_decoder(decoder).map_err(|e| Rejected::Corrupt(e.to_string()))?;
    img.apply_orientation(orientation);

    let transparent = img.color().has_alpha() && img.to_rgba8().pixels().any(|p| p.0[3] < 255);
    let mut out = Vec::new();
    for width in target_widths(img.width()) {
        let height = scaled_height(img.width(), img.height(), width);
        let resized = if width == img.width() {
            img.clone()
        } else {
            img.resize_exact(width, height, FilterType::CatmullRom)
        };
        out.extend(encode_all(&resized, transparent)?);
    }
    Ok(out)
}

/// Widths from [`WIDTHS`] below the original, plus the original width when it is smaller than
/// the largest step (so the full resolution is available without upscaling).
pub fn target_widths(original: u32) -> Vec<u32> {
    let max = WIDTHS.last().copied().unwrap_or(original);
    let mut widths: Vec<u32> = WIDTHS.iter().copied().filter(|w| *w < original).collect();
    widths.push(original.min(max));
    widths.dedup();
    widths
}

fn scaled_height(w: u32, h: u32, target: u32) -> u32 {
    let scaled = (u64::from(h) * u64::from(target) + u64::from(w) / 2) / u64::from(w.max(1));
    u32::try_from(scaled).unwrap_or(u32::MAX).max(1)
}

fn encode_all(img: &DynamicImage, transparent: bool) -> Result<Vec<Encoded>, Rejected> {
    let (w, h) = (img.width(), img.height());
    let fail = |e: image::ImageError| Rejected::Corrupt(e.to_string());
    let mut out = Vec::with_capacity(3);

    let avif = ravif::Encoder::new()
        .with_quality(AVIF_QUALITY)
        .with_speed(AVIF_SPEED)
        .with_num_threads(Some(1));
    let (width, height) = (
        usize::try_from(w).map_err(|e| Rejected::Corrupt(e.to_string()))?,
        usize::try_from(h).map_err(|e| Rejected::Corrupt(e.to_string()))?,
    );
    let avif_fail = |e: ravif::Error| Rejected::Corrupt(e.to_string());
    let avif = if transparent {
        let px: Vec<ravif::RGBA8> = img
            .to_rgba8()
            .pixels()
            .map(|p| ravif::RGBA8::new(p.0[0], p.0[1], p.0[2], p.0[3]))
            .collect();
        avif.encode_rgba(ravif::Img::new(&px[..], width, height))
            .map_err(avif_fail)?
    } else {
        let px: Vec<ravif::RGB8> = img
            .to_rgb8()
            .pixels()
            .map(|p| ravif::RGB8::new(p.0[0], p.0[1], p.0[2]))
            .collect();
        avif.encode_rgb(ravif::Img::new(&px[..], width, height))
            .map_err(avif_fail)?
    }
    .avif_file;
    out.push(Encoded {
        width: w,
        height: h,
        format: Format::Avif,
        bytes: avif,
    });

    let webp = if transparent {
        let rgba = img.to_rgba8();
        webp::Encoder::from_rgba(&rgba, w, h)
            .encode(WEBP_QUALITY)
            .to_vec()
    } else {
        let rgb = img.to_rgb8();
        webp::Encoder::from_rgb(&rgb, w, h)
            .encode(WEBP_QUALITY)
            .to_vec()
    };
    out.push(Encoded {
        width: w,
        height: h,
        format: Format::Webp,
        bytes: webp,
    });

    let mut fallback = Vec::new();
    let format = if transparent {
        let rgba = img.to_rgba8();
        PngEncoder::new(&mut fallback)
            .write_image(&rgba, w, h, image::ExtendedColorType::Rgba8)
            .map_err(fail)?;
        Format::Png
    } else {
        let rgb = img.to_rgb8();
        JpegEncoder::new_with_quality(&mut fallback, JPEG_QUALITY)
            .write_image(&rgb, w, h, image::ExtendedColorType::Rgb8)
            .map_err(fail)?;
        Format::Jpeg
    };
    out.push(Encoded {
        width: w,
        height: h,
        format,
        bytes: fallback,
    });
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use image::{Rgb, RgbImage, Rgba, RgbaImage};

    pub(crate) fn jpeg(w: u32, h: u32) -> Vec<u8> {
        let img = RgbImage::from_fn(w, h, |x, y| Rgb([(x % 256) as u8, (y % 256) as u8, 128]));
        let mut buf = Vec::new();
        JpegEncoder::new_with_quality(&mut buf, 90)
            .write_image(&img, w, h, image::ExtendedColorType::Rgb8)
            .unwrap();
        buf
    }

    /// A JPEG with an EXIF APP1 segment: Orientation = 6 (rotate 90° clockwise) and a
    /// fake GPS marker string.
    pub(crate) fn jpeg_with_exif(w: u32, h: u32) -> Vec<u8> {
        let plain = jpeg(w, h);
        // Little-endian TIFF, one IFD entry: tag 0x0112 (Orientation), SHORT, count 1, value 6.
        let mut tiff = b"II*\0\x08\0\0\0".to_vec();
        tiff.extend([1, 0]);
        tiff.extend([0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0]);
        tiff.extend([0, 0, 0, 0]);
        tiff.extend(b"GPSSECRET");
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend(tiff);
        let len = u16::try_from(app1.len() + 2).unwrap().to_be_bytes();
        let mut out = vec![0xFF, 0xD8, 0xFF, 0xE1, len[0], len[1]];
        out.extend(app1);
        out.extend(&plain[2..]);
        out
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for b in data {
            crc ^= u32::from(*b);
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
    fn widths_never_upscale() {
        assert_eq!(
            target_widths(4000),
            vec![160, 320, 480, 640, 720, 960, 1280, 1920]
        );
        assert_eq!(
            target_widths(1000),
            vec![160, 320, 480, 640, 720, 960, 1000]
        );
        assert_eq!(target_widths(640), vec![160, 320, 480, 640]);
        assert_eq!(target_widths(100), vec![100]);
        assert_eq!(scaled_height(4000, 3000, 160), 120);
        assert_eq!(scaled_height(3000, 1, 160), 1);
    }

    #[test]
    fn verify_sniffs_and_bounds() {
        let v = verify(&jpeg(40, 30)).unwrap();
        assert_eq!((v.mime, v.width, v.height), ("image/jpeg", 40, 30));
        assert_eq!(
            verify(b"<svg xmlns='http://www.w3.org/2000/svg'/>").unwrap_err(),
            Rejected::NotAnImage
        );
        let pdf = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n1 0 obj".to_vec();
        assert_eq!(verify(&pdf).unwrap_err().code(), "unsupported_type");
        // A PNG header claiming 20000 x 20000 px is refused before any decoding.
        let mut png = Vec::new();
        PngEncoder::new(&mut png)
            .write_image(&[0, 0, 0], 1, 1, image::ExtendedColorType::Rgb8)
            .unwrap();
        png[16..20].copy_from_slice(&20_000u32.to_be_bytes());
        png[20..24].copy_from_slice(&20_000u32.to_be_bytes());
        let crc = crc32(&png[12..29]).to_be_bytes();
        png[29..33].copy_from_slice(&crc);
        assert_eq!(verify(&png).unwrap_err(), Rejected::TooManyPixels);
        let mut truncated = jpeg(40, 30);
        truncated.truncate(12);
        assert!(verify(&truncated).is_err());
    }

    #[test]
    fn render_applies_orientation_and_strips_metadata() {
        let original = jpeg_with_exif(64, 32);
        assert!(original.windows(9).any(|w| w == b"GPSSECRET"));
        let out = render(&original).unwrap();
        // One width (64 < 160) rotated to 32 x 64, in three formats.
        let formats: Vec<Format> = out.iter().map(|e| e.format).collect();
        assert_eq!(formats, vec![Format::Avif, Format::Webp, Format::Jpeg]);
        for e in &out {
            assert_eq!((e.width, e.height), (32, 64), "{:?}", e.format);
            assert!(
                !e.bytes.windows(4).any(|w| w == b"Exif"),
                "{:?} has EXIF",
                e.format
            );
            assert!(!e.bytes.windows(9).any(|w| w == b"GPSSECRET"));
            let decoded = image::load_from_memory(&e.bytes);
            if e.format != Format::Avif {
                // No AVIF decoder is compiled in (it would need dav1d); others round-trip.
                assert_eq!(decoded.unwrap().width(), 32);
            }
            assert_eq!(infer::get(&e.bytes).unwrap().mime_type(), e.format.mime());
        }
    }

    #[test]
    fn transparent_images_fall_back_to_png() {
        let img = RgbaImage::from_fn(300, 200, |x, _| {
            Rgba([255, 0, 0, if x < 150 { 0 } else { 255 }])
        });
        let mut png = Vec::new();
        PngEncoder::new(&mut png)
            .write_image(&img, 300, 200, image::ExtendedColorType::Rgba8)
            .unwrap();
        let out = render(&png).unwrap();
        let widths: Vec<u32> = out.iter().map(|e| e.width).collect();
        assert_eq!(widths, vec![160, 160, 160, 300, 300, 300]);
        assert_eq!(out[2].format, Format::Png);
        assert_eq!(out[1].height, 107);
    }
}
