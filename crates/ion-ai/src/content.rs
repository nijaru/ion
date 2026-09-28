use base64::{Engine, engine::general_purpose::STANDARD};
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits, metadata::Orientation};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Cursor;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
    /// Original provider fragment when arguments were not a JSON object.
    /// Hosts must return a tool error without dispatching such a call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_arguments: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub name: String,
    pub result: Value,
    #[serde(default)]
    pub is_error: bool,
}

/// Validated image bytes encoded as base64 for durable, provider-neutral replay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageContent {
    mime_type: ImageMime,
    data: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageMime {
    #[serde(rename = "image/jpeg")]
    Jpeg,
    #[serde(rename = "image/png")]
    Png,
    #[serde(rename = "image/gif")]
    Gif,
    #[serde(rename = "image/webp")]
    Webp,
}

impl ImageMime {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
        }
    }

    fn detect(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Self::Png)
        } else if bytes.starts_with(b"\xff\xd8\xff") {
            Some(Self::Jpeg)
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            Some(Self::Gif)
        } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
            Some(Self::Webp)
        } else {
            None
        }
    }

    fn format(self) -> ImageFormat {
        match self {
            Self::Jpeg => ImageFormat::Jpeg,
            Self::Png => ImageFormat::Png,
            Self::Gif => ImageFormat::Gif,
            Self::Webp => ImageFormat::WebP,
        }
    }
}

#[derive(Debug, Error)]
pub enum ImageContentError {
    #[error("unsupported image; use JPEG, PNG, GIF or WebP")]
    Unsupported,
    #[error("invalid base64 image data")]
    InvalidEncoding(#[from] base64::DecodeError),
    #[error("image data does not match its MIME type")]
    MimeMismatch,
    #[error("invalid or oversized image: {0}")]
    Decode(#[from] image::ImageError),
    #[error("decoded image exceeds 128 MiB")]
    DecodedTooLarge,
}

impl ImageContent {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ImageContentError> {
        let mime_type = ImageMime::detect(bytes).ok_or(ImageContentError::Unsupported)?;
        Self::decode_source(bytes)?;
        Ok(Self {
            mime_type,
            data: STANDARD.encode(bytes),
        })
    }

    /// Decode a supported image under Ion's shared input resource bounds.
    /// The caller may apply the returned EXIF orientation before re-encoding.
    pub fn decode_source(bytes: &[u8]) -> Result<(DynamicImage, Orientation), ImageContentError> {
        let mime_type = ImageMime::detect(bytes).ok_or(ImageContentError::Unsupported)?;
        let mut reader = ImageReader::new(Cursor::new(bytes));
        reader.set_format(mime_type.format());
        let mut limits = Limits::default();
        limits.max_image_width = Some(8_000);
        limits.max_image_height = Some(8_000);
        limits.max_alloc = Some(128 * 1024 * 1024);
        reader.limits(limits);
        let mut decoder = reader.into_decoder()?;
        if decoder.total_bytes() > 128 * 1024 * 1024 {
            return Err(ImageContentError::DecodedTooLarge);
        }
        let orientation = decoder.orientation()?;
        Ok((DynamicImage::from_decoder(decoder)?, orientation))
    }

    #[must_use]
    pub fn mime_type(&self) -> ImageMime {
        self.mime_type
    }

    #[must_use]
    pub fn data(&self) -> &str {
        &self.data
    }

    pub fn validate(&self) -> Result<usize, ImageContentError> {
        let bytes = STANDARD.decode(&self.data)?;
        if ImageMime::detect(&bytes) != Some(self.mime_type) {
            return Err(ImageContentError::MimeMismatch);
        }
        Ok(bytes.len())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Content {
    Text(String),
    Image(ImageContent),
    ToolCall(ToolCall),
    ToolResult(ToolResult),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_input_checks_encoded_content_and_mime() {
        let data = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";
        let bytes = STANDARD.decode(data).unwrap();
        let image = ImageContent::from_bytes(&bytes).unwrap();
        assert_eq!(image.mime_type(), ImageMime::Png);
        assert_eq!(image.validate().unwrap(), bytes.len());
        let mut corrupt = bytes;
        corrupt[45] ^= 0xff;
        assert!(ImageContent::from_bytes(&corrupt).is_err());
        let mismatch: ImageContent = serde_json::from_value(serde_json::json!({
            "mime_type":"image/jpeg", "data": data
        }))
        .unwrap();
        assert!(matches!(
            mismatch.validate(),
            Err(ImageContentError::MimeMismatch)
        ));
    }
}
