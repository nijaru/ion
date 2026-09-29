//! File-backed user images, normalized before a coding Turn is accepted.
use std::{
    fs,
    io::{Cursor, Read},
    path::Path,
};

use anyhow::{Context, Result, bail, ensure};
use image::{
    DynamicImage, GenericImageView, ImageFormat, RgbaImage, codecs::jpeg::JpegEncoder,
    imageops::FilterType, metadata::Orientation,
};
use ion_ai::{Content, ImageContent};

use crate::Selection;

// Inline images share the existing 8 MiB request bound with instructions and
// conversation. Accept larger source files when they can be normalized first.
const MAX_SOURCE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_INLINE_BYTES: usize = 5 * 1024 * 1024;
const MAX_EDGE: u32 = 2_000;

#[derive(Clone)]
pub struct LoadedImage {
    pub content: ImageContent,
    /// Shown to the model when dimensions changed, so coordinates refer to
    /// the image that was actually sent rather than the original file.
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

pub fn load_image(selected: &Selection, path: &Path) -> Result<LoadedImage> {
    require_image_input(selected)?;
    let metadata =
        fs::metadata(path).with_context(|| format!("cannot inspect {}", path.display()))?;
    ensure!(
        metadata.is_file(),
        "{} is not a regular file",
        path.display()
    );
    ensure!(
        metadata.len() <= MAX_SOURCE_BYTES,
        "{} exceeds Ion's current 32 MiB source-image bound",
        path.display()
    );
    let mut bytes = Vec::new();
    fs::File::open(path)
        .with_context(|| format!("cannot open {}", path.display()))?
        .take(MAX_SOURCE_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("cannot read {}", path.display()))?;
    ensure!(
        bytes.len() as u64 <= MAX_SOURCE_BYTES,
        "{} changed beyond Ion's current 32 MiB source-image bound",
        path.display()
    );
    normalize(&bytes).with_context(|| format!("invalid image {}", path.display()))
}

/// Normalize an RGBA image obtained from the local clipboard without a
/// temporary file. The same capability and inline bounds apply as file input.
pub fn load_rgba(
    selected: &Selection,
    width: usize,
    height: usize,
    rgba: Vec<u8>,
) -> Result<LoadedImage> {
    require_image_input(selected)?;
    normalize_rgba(width, height, rgba)
}

fn require_image_input(selected: &Selection) -> Result<()> {
    ensure!(
        selected.image_input,
        "{}/{} does not declare image input; choose an image-capable model or configure the custom route with --images",
        selected.provider,
        selected.model
    );
    Ok(())
}

fn normalize_rgba(width: usize, height: usize, rgba: Vec<u8>) -> Result<LoadedImage> {
    ensure!(
        width > 0 && height > 0,
        "clipboard image has empty dimensions"
    );
    ensure!(
        width <= 8_000 && height <= 8_000,
        "clipboard image dimensions exceed 8000 pixels"
    );
    let expected = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .context("clipboard image dimensions overflow")?;
    ensure!(
        expected <= 128 * 1024 * 1024,
        "clipboard image exceeds 128 MiB decoded bound"
    );
    ensure!(
        rgba.len() == expected,
        "clipboard image has invalid RGBA data length"
    );
    let image = RgbaImage::from_raw(width as u32, height as u32, rgba)
        .context("clipboard image has invalid RGBA data")?;
    normalize_decoded(
        DynamicImage::ImageRgba8(image),
        Orientation::NoTransforms,
        None,
    )
}

fn normalize(bytes: &[u8]) -> Result<LoadedImage> {
    let (image, orientation) = ImageContent::decode_source(bytes)?;
    normalize_decoded(image, orientation, Some(bytes))
}

fn normalize_decoded(
    mut image: DynamicImage,
    orientation: Orientation,
    original_bytes: Option<&[u8]>,
) -> Result<LoadedImage> {
    let (original_width, original_height) = image.dimensions();
    image.apply_orientation(orientation);
    let (display_width, display_height) = image.dimensions();
    if orientation == Orientation::NoTransforms
        && display_width <= MAX_EDGE
        && display_height <= MAX_EDGE
        && let Some(bytes) = original_bytes.filter(|bytes| bytes.len() <= MAX_INLINE_BYTES)
        && let Ok(content) = ImageContent::from_bytes(bytes)
    {
        return Ok(LoadedImage {
            content,
            note: None,
        });
    }

    let mut current = if display_width > MAX_EDGE || display_height > MAX_EDGE {
        image.resize(MAX_EDGE, MAX_EDGE, FilterType::Lanczos3)
    } else {
        image
    };
    loop {
        let encoded = encode_inline(&current)?;
        if let Some(bytes) = encoded {
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
            bail!("image cannot fit the inline request bound");
        }
        current = current.resize(
            (width * 3 / 4).max(1),
            (height * 3 / 4).max(1),
            FilterType::Lanczos3,
        );
    }
}

fn encode_inline(image: &DynamicImage) -> Result<Option<Vec<u8>>> {
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
    use base64::Engine;
    use image::RgbImage;

    #[test]
    fn large_image_is_resized_and_remains_replayable() {
        let mut state = 1u32;
        let pixels = RgbImage::from_fn(2_100, 2_100, |_, _| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            image::Rgb(state.to_le_bytes()[..3].try_into().unwrap())
        });
        let mut source = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(pixels)
            .write_to(&mut source, ImageFormat::Png)
            .unwrap();
        let loaded = normalize(&source.into_inner()).unwrap();
        assert!(
            loaded
                .note
                .as_deref()
                .unwrap()
                .contains("originally 2100x2100")
        );
        assert!(loaded.content.validate().unwrap() <= MAX_INLINE_BYTES);
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(loaded.content.data())
            .unwrap();
        let (width, height) = image::load_from_memory(&bytes).unwrap().dimensions();
        assert_eq!(width, height);
        assert!(width <= MAX_EDGE);
    }

    #[test]
    fn clipboard_rgba_is_bounded_and_replayable() {
        let loaded = normalize_rgba(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 255]).unwrap();
        assert_eq!(loaded.content.mime_type().as_str(), "image/png");
        assert!(loaded.content.validate().is_ok());
        assert!(normalize_rgba(2, 1, vec![0; 4]).is_err());
        assert!(normalize_rgba(8_001, 1, vec![]).is_err());
    }
}
