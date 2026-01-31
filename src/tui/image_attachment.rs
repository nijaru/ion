//! Image attachment parsing for @image:path syntax.

use crate::provider::ContentBlock;
use base64::Engine;
use std::path::Path;

/// Pattern for image attachments: @image:path/to/file.png
const IMAGE_PREFIX: &str = "@image:";

/// Supported image extensions and their media types.
const SUPPORTED_FORMATS: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
];

/// Parse input text and extract image attachments.
/// Returns a list of content blocks (text and images).
///
/// Supports:
/// - `@image:path.png` - simple paths without spaces
/// - `@image:"path with spaces.png"` - quoted paths
///
/// Example input: "Look at this @image:screenshot.png and tell me what you see"
/// Returns: [Text("Look at this "), Image(...), Text(" and tell me what you see")]
pub fn parse_image_attachments(input: &str, working_dir: &Path) -> Vec<ContentBlock> {
    let mut blocks = Vec::new();
    let mut remaining = input;
    let mut current_text = String::new();

    while let Some(pos) = remaining.find(IMAGE_PREFIX) {
        // Add text before the @image: marker
        current_text.push_str(&remaining[..pos]);

        // Find the end of the path - handle quoted paths for spaces
        let after_prefix = &remaining[pos + IMAGE_PREFIX.len()..];
        let (path_str, consumed) = if after_prefix.starts_with('"') {
            // Quoted path: find closing quote
            let after_quote = &after_prefix[1..];
            if let Some(end_quote) = after_quote.find('"') {
                (&after_quote[..end_quote], end_quote + 2) // +2 for both quotes
            } else {
                // No closing quote - treat rest as path
                (after_quote, after_quote.len() + 1)
            }
        } else {
            // Unquoted path: ends at whitespace
            let path_end = after_prefix
                .find(char::is_whitespace)
                .unwrap_or(after_prefix.len());
            (&after_prefix[..path_end], path_end)
        };

        // Try to load the image
        let full_path = if Path::new(path_str).is_absolute() {
            path_str.to_string()
        } else {
            working_dir.join(path_str).to_string_lossy().to_string()
        };

        match load_image(&full_path) {
            Ok((media_type, data)) => {
                // Flush current text as a block
                if !current_text.is_empty() {
                    blocks.push(ContentBlock::Text {
                        text: current_text.clone(),
                    });
                    current_text.clear();
                }
                // Add image block
                blocks.push(ContentBlock::Image { media_type, data });
            }
            Err(e) => {
                // On error, keep the original text and add error note
                current_text.push_str(&format!("[Image error: {e}]"));
            }
        }

        // Continue with rest of string
        remaining = &after_prefix[consumed..];
    }

    // Add remaining text
    current_text.push_str(remaining);
    if !current_text.is_empty() {
        blocks.push(ContentBlock::Text { text: current_text });
    }

    // If no images found, just return a single text block
    if blocks.is_empty() {
        blocks.push(ContentBlock::Text {
            text: input.to_string(),
        });
    }

    blocks
}

/// Load an image file and return (media_type, base64_data).
fn load_image(path: &str) -> Result<(String, String), String> {
    let path = Path::new(path);

    // Check extension
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .ok_or_else(|| "No file extension".to_string())?;

    let media_type = SUPPORTED_FORMATS
        .iter()
        .find(|(e, _)| *e == ext)
        .map(|(_, mt)| *mt)
        .ok_or_else(|| format!("Unsupported format: {ext}"))?;

    // Check file size BEFORE reading (prevents OOM with large files)
    const MAX_SIZE: u64 = 20 * 1024 * 1024;
    let metadata = std::fs::metadata(path).map_err(|e| format!("Failed to stat: {e}"))?;
    if metadata.len() > MAX_SIZE {
        return Err(format!(
            "Image too large: {} bytes (max {})",
            metadata.len(),
            MAX_SIZE
        ));
    }

    // Read file (now safe, size is bounded)
    let data = std::fs::read(path).map_err(|e| format!("Failed to read: {e}"))?;

    // Base64 encode
    let encoded = base64::engine::general_purpose::STANDARD.encode(&data);

    Ok((media_type.to_string(), encoded))
}

/// Check if input contains image attachments.
#[must_use]
pub fn has_image_attachments(input: &str) -> bool {
    input.contains(IMAGE_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_test_image() -> (TempDir, String) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.png");
        // Minimal valid PNG (1x1 transparent pixel)
        let png_data: Vec<u8> = vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
            0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR chunk
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // 1x1
            0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89, // 8-bit RGBA
            0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, // IDAT chunk
            0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, // compressed data
            0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, // IEND chunk
            0xAE, 0x42, 0x60, 0x82,
        ];
        fs::write(&path, &png_data).unwrap();
        (dir, path.to_string_lossy().to_string())
    }

    #[test]
    fn test_no_images() {
        let blocks = parse_image_attachments("Hello world", Path::new("."));
        assert_eq!(blocks.len(), 1);
        assert!(matches!(&blocks[0], ContentBlock::Text { text } if text == "Hello world"));
    }

    #[test]
    fn test_has_image_attachments() {
        assert!(!has_image_attachments("Hello world"));
        assert!(has_image_attachments("Look at @image:test.png"));
    }

    #[test]
    fn test_parse_with_valid_image() {
        let (dir, path) = setup_test_image();
        let input = format!("Look at @image:{path} please");
        let blocks = parse_image_attachments(&input, dir.path());

        assert_eq!(blocks.len(), 3);
        assert!(matches!(&blocks[0], ContentBlock::Text { text } if text == "Look at "));
        assert!(
            matches!(&blocks[1], ContentBlock::Image { media_type, .. } if media_type == "image/png")
        );
        assert!(matches!(&blocks[2], ContentBlock::Text { text } if text == " please"));
    }

    #[test]
    fn test_parse_with_missing_image() {
        let blocks = parse_image_attachments("Look at @image:missing.png", Path::new("."));
        assert_eq!(blocks.len(), 1);
        assert!(matches!(&blocks[0], ContentBlock::Text { text } if text.contains("Image error")));
    }

    #[test]
    fn test_unsupported_format() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.bmp");
        fs::write(&path, b"fake bmp").unwrap();

        let input = format!("Look at @image:{}", path.display());
        let blocks = parse_image_attachments(&input, dir.path());

        assert_eq!(blocks.len(), 1);
        assert!(
            matches!(&blocks[0], ContentBlock::Text { text } if text.contains("Unsupported format"))
        );
    }

    #[test]
    fn test_quoted_path_with_spaces() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("my screenshot.png");
        // Minimal valid PNG
        let png_data: Vec<u8> = vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x49,
            0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        fs::write(&path, &png_data).unwrap();

        let input = format!("Look at @image:\"{}\" please", path.display());
        let blocks = parse_image_attachments(&input, dir.path());

        assert_eq!(blocks.len(), 3);
        assert!(matches!(&blocks[0], ContentBlock::Text { text } if text == "Look at "));
        assert!(
            matches!(&blocks[1], ContentBlock::Image { media_type, .. } if media_type == "image/png")
        );
        assert!(matches!(&blocks[2], ContentBlock::Text { text } if text == " please"));
    }
}
