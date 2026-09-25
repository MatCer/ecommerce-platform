//! Responsive images for page models. The media pipeline stores content-addressed variants in
//! the public bucket (`media/<tenant>/<asset>/<sha256>.<ext>`); the edge serves that bucket
//! under the shop's own origin (`/media/...`), so URLs here are same-origin paths and the
//! theme CSP (`img-src 'self'`) holds.

use serde::Serialize;
use utoipa::ToSchema;

use crate::media::AssetVariant;

/// An image with every generated width. Render `srcset` (AVIF) with a `sizes` that matches the
/// real slot; `src` is a JPEG/PNG fallback. `width`/`height` are the intrinsic size of the
/// largest variant (use them for the aspect ratio: no layout shift).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Image {
    pub alt: String,
    pub width: u32,
    pub height: u32,
    /// Fallback (JPEG or PNG) around 640 px wide.
    pub src: String,
    /// AVIF candidates: `"/media/... 320w, /media/... 640w"`.
    pub srcset: String,
    /// WebP candidates, for `<picture>` fallbacks.
    pub srcset_webp: String,
    /// Fallback-format candidates.
    pub srcset_fallback: String,
}

fn url(key: &str) -> String {
    format!("/{key}")
}

fn srcset(variants: &[&AssetVariant]) -> String {
    variants
        .iter()
        .map(|v| format!("{} {}w", url(&v.key), v.width))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Builds the image from an asset's variants; `None` when it has none (not processed yet).
pub fn from_variants(variants: &[AssetVariant], alt: String) -> Option<Image> {
    let pick = |formats: &[&str]| {
        let mut v: Vec<&AssetVariant> = variants
            .iter()
            .filter(|v| formats.contains(&v.format.as_str()))
            .collect();
        v.sort_by_key(|v| v.width);
        v
    };
    let avif = pick(&["avif"]);
    let webp = pick(&["webp"]);
    let fallback = pick(&["jpeg", "png"]);
    let largest = fallback.last().or(avif.last())?;
    let src = fallback
        .iter()
        .rev()
        .find(|v| v.width <= 640)
        .or(fallback.first())
        .or(avif.first())?;
    Some(Image {
        alt,
        width: largest.width,
        height: largest.height,
        src: url(&src.key),
        srcset: srcset(&avif),
        srcset_webp: srcset(&webp),
        srcset_fallback: srcset(&fallback),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(width: u32, format: &str) -> AssetVariant {
        AssetVariant {
            width,
            height: width * 5 / 4,
            format: format.into(),
            key: format!("media/t/a/{width}.{format}"),
            bytes: 1,
            url: String::new(),
        }
    }

    #[test]
    fn srcsets_per_format_sorted_by_width() {
        let img = from_variants(
            &[
                v(960, "avif"),
                v(320, "avif"),
                v(320, "webp"),
                v(960, "jpeg"),
                v(320, "jpeg"),
                v(640, "jpeg"),
            ],
            "Tričko".into(),
        )
        .unwrap();
        assert_eq!(
            img.srcset,
            "/media/t/a/320.avif 320w, /media/t/a/960.avif 960w"
        );
        assert_eq!(img.srcset_webp, "/media/t/a/320.webp 320w");
        assert_eq!(img.src, "/media/t/a/640.jpeg");
        assert_eq!((img.width, img.height), (960, 1200));
        assert_eq!(img.alt, "Tričko");
    }

    #[test]
    fn no_variants_no_image() {
        assert!(from_variants(&[], String::new()).is_none());
    }
}
