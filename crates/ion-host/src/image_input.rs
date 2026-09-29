//! Host-facing image input. Capability and filesystem access belong here;
//! normalization is shared with images returned by coding tools.
use std::{fs, io::Read, path::Path};

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use ion_ai::{ImageMime, MAX_SOURCE_BYTES, normalize_image, normalize_rgba};

use crate::Selection;

pub use ion_ai::LoadedImage;

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
        metadata.len() <= MAX_SOURCE_BYTES as u64,
        "{} exceeds Ion's current 32 MiB source-image bound",
        path.display()
    );
    let mut bytes = Vec::new();
    fs::File::open(path)
        .with_context(|| format!("cannot open {}", path.display()))?
        .take(MAX_SOURCE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("cannot read {}", path.display()))?;
    normalize_image(&bytes).with_context(|| format!("invalid image {}", path.display()))
}

pub fn load_encoded_image(
    selected: &Selection,
    mime_type: &str,
    data: &str,
) -> Result<LoadedImage> {
    require_image_input(selected)?;
    let declared = ImageMime::parse(mime_type)
        .ok_or_else(|| anyhow::anyhow!("unsupported inline image MIME type"))?;
    ensure!(
        data.len() <= MAX_SOURCE_BYTES.div_ceil(3) * 4,
        "inline image exceeds Ion's current 32 MiB source-image bound"
    );
    let bytes = STANDARD.decode(data).context("invalid base64 image data")?;
    ensure!(
        ImageMime::detect(&bytes) == Some(declared),
        "image data does not match its MIME type"
    );
    normalize_image(&bytes).context("invalid inline image")
}

pub fn load_rgba(
    selected: &Selection,
    width: usize,
    height: usize,
    rgba: Vec<u8>,
) -> Result<LoadedImage> {
    require_image_input(selected)?;
    normalize_rgba(width, height, rgba).context("invalid clipboard image")
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
