//! Provider-neutral preparation for images retained in a Session.
use std::io::Cursor;

use image::{
    DynamicImage, GenericImageView, ImageFormat, RgbaImage, codecs::jpeg::JpegEncoder,
    imageops::FilterType, metadata::Orientation,
};
use thiserror::Error;

use crate::{Content, ImageContent, ImageContentError, content::MAX_INLINE_BYTES};

pub const MAX_SOURCE_BYTES: usize = 32 * 1024 * 1024;
const MAX_EDGE: u32 = 2_000;

#[derive(Debug, Error)]
pub enum ImagePreparationError {
    #[error("image exceeds 32 MiB source bound")]
    SourceTooLarge,
    #[error("image dimensions or RGBA data are invalid or exceed the decoded bound")]
    InvalidRgba,
    #[error("image cannot fit the inline request bound")]
    CannotFit,
    #[error(transparent)]
    Content(#[from] ImageContentError),
    #[error(transparent)]
    Encode(#[from] image::ImageError),
}

#[derive(Clone)]
pub struct LoadedImage {
    pub content: ImageContent,
    /// Coordinates in a model response must refer to the image actually sent.
    pub note: Option<String>,
}

impl LoadedImage {
    pub fn into_parts(self) -> Vec<Content> {
        let mut parts = Vec::with_capacity(2);
        if let Some(note) = self.note {
            parts.push(Content::Text(note));
        }
        parts.push(Content::Image(self.content));
        parts
    }
}

pub fn normalize_image(bytes: &[u8]) -> Result<LoadedImage, ImagePreparationError> {
    if bytes.len() > MAX_SOURCE_BYTES {
        return Err(ImagePreparationError::SourceTooLarge);
    }
    let (image, orientation) = ImageContent::decode_source(bytes)?;
    normalize_decoded(image, orientation, Some(bytes))
}

pub fn normalize_rgba(
    width: usize,
    height: usize,
    rgba: Vec<u8>,
) -> Result<LoadedImage, ImagePreparationError> {
    let expected = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4));
    if width == 0
        || height == 0
        || width > 8_000
        || height > 8_000
        || expected.is_none_or(|len| len > 128 * 1024 * 1024 || len != rgba.len())
    {
        return Err(ImagePreparationError::InvalidRgba);
    }
    let image = RgbaImage::from_raw(width as u32, height as u32, rgba)
        .ok_or(ImagePreparationError::InvalidRgba)?;
    normalize_decoded(
        DynamicImage::ImageRgba8(image),
        Orientation::NoTransforms,
        None,
    )
}

fn normalize_decoded(
    mut image: DynamicImage,
    orientation: Orientation,
    original_bytes: Option<&[u8]>,
) -> Result<LoadedImage, ImagePreparationError> {
    let (original_width, original_height) = image.dimensions();
    image.apply_orientation(orientation);
    let (display_width, display_height) = image.dimensions();
    if orientation == Orientation::NoTransforms
        && display_width <= MAX_EDGE
        && display_height <= MAX_EDGE
        && let Some(bytes) = original_bytes.filter(|bytes| bytes.len() <= MAX_INLINE_BYTES)
    {
        return Ok(LoadedImage {
            content: ImageContent::from_bytes(bytes)?,
            note: None,
        });
    }

    let mut current = if display_width > MAX_EDGE || display_height > MAX_EDGE {
        image.resize(MAX_EDGE, MAX_EDGE, FilterType::Lanczos3)
    } else {
        image
    };
    loop {
        if let Some(bytes) = encode_inline(&current)? {
            let (width, height) = current.dimensions();
            let note = if (width, height) == (original_width, original_height)
                && orientation == Orientation::NoTransforms
            {
                None
            } else {
                Some(format!(
                    "[Image originally {original_width}x{original_height}; sent at {width}x{height}. Use sent dimensions for coordinates.]"
                ))
            };
            return Ok(LoadedImage {
                content: ImageContent::from_bytes(&bytes)?,
                note,
            });
        }
        let (width, height) = current.dimensions();
        if width == 1 && height == 1 {
            return Err(ImagePreparationError::CannotFit);
        }
        current = current.resize(
            (width * 3 / 4).max(1),
            (height * 3 / 4).max(1),
            FilterType::Lanczos3,
        );
    }
}

fn encode_inline(image: &DynamicImage) -> Result<Option<Vec<u8>>, ImagePreparationError> {
    let mut png = Cursor::new(Vec::new());
    image.write_to(&mut png, ImageFormat::Png)?;
    let png = png.into_inner();
    if png.len() <= MAX_INLINE_BYTES {
        return Ok(Some(png));
    }
    if !image.color().has_alpha() {
        let mut jpeg = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg, 85).encode_image(image)?;
        if jpeg.len() <= MAX_INLINE_BYTES {
            return Ok(Some(jpeg));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::RgbImage;

    #[test]
    fn clipboard_image_normalization_checks_shape_and_returns_replayable_png() {
        let loaded = normalize_rgba(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 255]).unwrap();
        assert_eq!(loaded.content.mime_type().as_str(), "image/png");
        assert!(loaded.content.validate().is_ok());
        assert!(normalize_rgba(2, 1, vec![0; 4]).is_err());
        assert!(normalize_rgba(8_001, 1, vec![]).is_err());
    }

    #[test]
    fn oversized_dimensions_are_resized_with_a_coordinate_note() {
        let pixels = RgbImage::from_pixel(2_100, 2_100, image::Rgb([255, 0, 0]));
        let mut source = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(pixels)
            .write_to(&mut source, ImageFormat::Png)
            .unwrap();
        let loaded = normalize_image(&source.into_inner()).unwrap();
        assert!(
            loaded
                .note
                .as_deref()
                .unwrap()
                .contains("sent at 2000x2000")
        );
        assert!(loaded.content.validate().unwrap() <= MAX_INLINE_BYTES);
    }
}
