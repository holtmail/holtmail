//! Small preview images of image attachments.
//!
//! A thumbnail is derived data: callers cache it and can make it again from the attachment's
//! bytes at any time. Only PNG, JPEG, GIF and WebP are read, whatever the declared type says.

use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits};

/// The longest side of a thumbnail, in pixels.
pub const MAX_SIDE: u32 = 320;

/// Sources larger than this, in bytes, are refused.
pub const MAX_SOURCE: usize = 25 * 1024 * 1024;

/// Sources with more pixels than this are refused before they are decoded.
pub const MAX_PIXELS: u64 = 40_000_000;

const MAX_DIMENSION: u32 = 20_000;
const MAX_ALLOC: u64 = 256 * 1024 * 1024;
const JPEG_QUALITY: u8 = 80;

/// A thumbnail ready to serve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumbnail {
    /// `image/jpeg`, or `image/png` for images with transparency.
    pub content_type: &'static str,
    /// The encoded image.
    pub bytes: Vec<u8>,
}

/// Why no thumbnail was made. The same bytes always give the same answer.
#[derive(Debug, thiserror::Error)]
pub enum ThumbnailError {
    /// Not a PNG, JPEG, GIF or WebP image.
    #[error("not a supported image")]
    Unsupported,
    /// Larger than [`MAX_SOURCE`] bytes, [`MAX_PIXELS`] pixels or the decoder's memory limit.
    #[error("image too large")]
    TooLarge,
    /// A supported format that does not decode.
    #[error("unreadable image: {0}")]
    Unreadable(String),
}

/// Whether a declared media type is one [`thumbnail`] may read, so clients can be offered a
/// thumbnail without decoding anything.
pub fn supports(content_type: &str) -> bool {
    matches!(
        content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "image/png" | "image/jpeg" | "image/jpg" | "image/pjpeg" | "image/gif" | "image/webp"
    )
}

/// A thumbnail of `source` that fits in [`MAX_SIDE`] × [`MAX_SIDE`], keeping the aspect ratio and
/// turned upright by its EXIF orientation. Smaller images keep their size. Animated images give
/// their first frame. CPU-bound: run it off the async runtime.
pub fn thumbnail(source: &[u8]) -> Result<Thumbnail, ThumbnailError> {
    if source.len() > MAX_SOURCE {
        return Err(ThumbnailError::TooLarge);
    }
    let reader = || -> Result<ImageReader<Cursor<&[u8]>>, ThumbnailError> {
        let mut reader = ImageReader::new(Cursor::new(source))
            .with_guessed_format()
            .map_err(|e| ThumbnailError::Unreadable(e.to_string()))?;
        if !matches!(
            reader.format(),
            Some(ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::WebP)
        ) {
            return Err(ThumbnailError::Unsupported);
        }
        let mut limits = Limits::default();
        limits.max_image_width = Some(MAX_DIMENSION);
        limits.max_image_height = Some(MAX_DIMENSION);
        limits.max_alloc = Some(MAX_ALLOC);
        reader.limits(limits);
        Ok(reader)
    };
    let (width, height) = reader()?.into_dimensions().map_err(decode_error)?;
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(ThumbnailError::TooLarge);
    }
    let mut decoder = reader()?.into_decoder().map_err(decode_error)?;
    let orientation = decoder.orientation().ok();
    let mut image = DynamicImage::from_decoder(decoder).map_err(decode_error)?;
    if let Some(orientation) = orientation {
        image.apply_orientation(orientation);
    }
    if image.width() > MAX_SIDE || image.height() > MAX_SIDE {
        image = image.resize(MAX_SIDE, MAX_SIDE, FilterType::Triangle);
    }
    let mut bytes = Vec::new();
    let content_type = if image.color().has_alpha() {
        image
            .to_rgba8()
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .map_err(decode_error)?;
        "image/png"
    } else {
        JpegEncoder::new_with_quality(&mut bytes, JPEG_QUALITY)
            .encode_image(&image.to_rgb8())
            .map_err(decode_error)?;
        "image/jpeg"
    };
    Ok(Thumbnail {
        content_type,
        bytes,
    })
}

fn decode_error(error: image::ImageError) -> ThumbnailError {
    match error {
        image::ImageError::Limits(_) => ThumbnailError::TooLarge,
        image::ImageError::Unsupported(_) => ThumbnailError::Unsupported,
        other => ThumbnailError::Unreadable(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use image::{Rgb, RgbImage, Rgba, RgbaImage};

    use super::*;

    fn encode(image: DynamicImage, format: ImageFormat) -> Vec<u8> {
        let mut out = Vec::new();
        image.write_to(&mut Cursor::new(&mut out), format).unwrap();
        out
    }

    fn size(thumbnail: &Thumbnail) -> (u32, u32) {
        image::load_from_memory(&thumbnail.bytes)
            .unwrap()
            .to_rgba8()
            .dimensions()
    }

    #[test]
    fn shrinks_a_photo_to_a_jpeg_keeping_the_shape() {
        let photo = RgbImage::from_pixel(1200, 600, Rgb([10, 120, 200]));
        let thumb = thumbnail(&encode(photo.into(), ImageFormat::Png)).unwrap();
        assert_eq!(thumb.content_type, "image/jpeg");
        assert_eq!(size(&thumb), (320, 160));
    }

    #[test]
    fn keeps_transparency_as_png_and_small_images_as_they_are() {
        let icon = RgbaImage::from_pixel(40, 30, Rgba([0, 0, 0, 0]));
        let thumb = thumbnail(&encode(icon.into(), ImageFormat::Png)).unwrap();
        assert_eq!(thumb.content_type, "image/png");
        assert_eq!(size(&thumb), (40, 30));
    }

    #[test]
    fn reads_gif_and_jpeg() {
        let picture = RgbImage::from_pixel(500, 1000, Rgb([200, 30, 30]));
        for format in [ImageFormat::Gif, ImageFormat::Jpeg] {
            let thumb = thumbnail(&encode(picture.clone().into(), format)).unwrap();
            assert_eq!(size(&thumb), (160, 320), "{format:?}");
        }
    }

    #[test]
    fn refuses_what_it_should_not_read() {
        assert!(matches!(
            thumbnail(b"%PDF-1.5 not an image"),
            Err(ThumbnailError::Unsupported)
        ));
        let mut bmp = b"BM".to_vec();
        bmp.resize(64, 0);
        assert!(matches!(thumbnail(&bmp), Err(ThumbnailError::Unsupported)));
        let mut cut = encode(RgbImage::new(64, 64).into(), ImageFormat::Png);
        cut.truncate(60);
        assert!(matches!(
            thumbnail(&cut),
            Err(ThumbnailError::Unreadable(_))
        ));
    }

    #[test]
    fn refuses_huge_dimensions_before_decoding() {
        let mut png = encode(RgbImage::new(1, 1).into(), ImageFormat::Png);
        png[16..20].copy_from_slice(&30_000u32.to_be_bytes());
        png[20..24].copy_from_slice(&30_000u32.to_be_bytes());
        let crc = crc32(&png[12..29]);
        png[29..33].copy_from_slice(&crc.to_be_bytes());
        assert!(matches!(thumbnail(&png), Err(ThumbnailError::TooLarge)));
    }

    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
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
    fn knows_the_types_it_reads() {
        assert!(supports("IMAGE/JPEG; name=a.jpg"));
        assert!(supports("image/webp"));
        assert!(!supports("image/svg+xml"));
        assert!(!supports("application/pdf"));
    }
}
