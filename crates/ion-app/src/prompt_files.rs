//! Explicit file operands: bounded snapshots, never resource-command input.
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use ion_ai::{Content, ImageMime, LoadedImage, MAX_SOURCE_BYTES, Message, ModelRef};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

/// Literal UTF-8 context retained with an unaccepted prompt. Once committed it
/// is ordinary message text, not a live filesystem reference.
#[derive(Clone, Debug, Serialize)]
pub struct TextFile {
    pub name: String,
    pub text: String,
}

impl TextFile {
    fn model_text(&self) -> String {
        // JSON keeps filenames, controls and apparent closing delimiters inside
        // the data envelope. This is framing, not a prompt-injection guarantee.
        format!(
            "Attached file (UTF-8, JSON encoded):\n{}\n",
            serde_json::to_string(self).expect("strings serialize")
        )
    }
}

/// Resource expansion belongs before this composition and applies only to the
/// prompt, never to file contents. Saved history retains the complete snapshot.
pub fn compose_prompt(
    text: String,
    files: &[TextFile],
    images: impl IntoIterator<Item = LoadedImage>,
) -> Message {
    if files.is_empty() {
        return Message::user_input(text, images);
    }
    let mut composed = String::new();
    for file in files {
        composed.push_str(&file.model_text());
    }
    composed.push_str(&text);
    Message::user_input(composed, images)
}

#[derive(Default)]
pub struct PreparedFiles {
    pub text: Vec<TextFile>,
    pub images: Vec<LoadedImage>,
}

/// Read regular files (including symlinks to them) once. Images use the same
/// normalization and capability contract as explicit image attachments. The
/// blocking worker is always joined; cancellation discards its result.
pub async fn prepare_files(
    paths: Vec<PathBuf>,
    model: ModelRef,
    image_input: bool,
    max_payload_bytes: usize,
    stop: CancellationToken,
) -> Result<PreparedFiles> {
    ensure!(!stop.is_cancelled(), "file preparation cancelled");
    if paths.is_empty() {
        return Ok(PreparedFiles::default());
    }
    let worker_stop = stop.clone();
    let joined = tokio::task::spawn_blocking(move || {
        let mut prepared = PreparedFiles::default();
        let mut payload = 0usize;
        for path in paths {
            let name = path.to_str().context("file operand path is not UTF-8")?;
            ensure!(!worker_stop.is_cancelled(), "file preparation cancelled");
            let bytes = crate::file_io::read_bounded(&path, MAX_SOURCE_BYTES)
                .with_context(|| format!("cannot read file operand {path:?}"))?;
            if ImageMime::detect(&bytes).is_some() {
                crate::image_input::require_image_input(&model, image_input)?;
                let mut image = ion_ai::normalize_image(&bytes)
                    .with_context(|| format!("invalid image {path:?}"))?;
                let mut note = format!("Attached image file: {}", serde_json::to_string(name)?);
                if let Some(normalization) = image.note.take() {
                    note.push('\n');
                    note.push_str(&normalization);
                }
                image.note = Some(note);
                payload = payload
                    .checked_add(serde_json::to_vec(&image.content)?.len())
                    .and_then(|size| size.checked_add(image.note.as_ref().map_or(0, String::len)))
                    .context("file payload size overflow")?;
                ensure!(
                    payload <= max_payload_bytes,
                    "file operands exceed input byte limit"
                );
                prepared.images.push(image);
            } else {
                let text = String::from_utf8(bytes).with_context(|| {
                    format!("file operand {path:?} is not UTF-8 text or a supported image")
                })?;
                ensure!(
                    !text.contains('\0'),
                    "file operand {path:?} contains binary NUL bytes"
                );
                ensure!(
                    text.len() <= max_payload_bytes,
                    "file operands exceed input byte limit"
                );
                let text = text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned();
                let file = TextFile {
                    name: name.to_owned(),
                    text,
                };
                payload = payload
                    .checked_add(serde_json::to_vec(&Content::Text(file.model_text()))?.len())
                    .context("file payload size overflow")?;
                ensure!(
                    payload <= max_payload_bytes,
                    "file operands exceed input byte limit"
                );
                prepared.text.push(file);
            }
        }
        Ok(prepared)
    })
    .await;
    let result = match joined {
        Ok(result) => result?,
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(error) => return Err(error).context("file preparation worker stopped"),
    };
    ensure!(!stop.is_cancelled(), "file preparation cancelled");
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[tokio::test]
    async fn snapshots_preserve_literal_data_and_reject_invalid_or_oversized_sources() {
        let root = std::env::temp_dir().join(format!("ion-files-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("name\"\n🦀.md");
        let text = "/not-a-template {{ARG}}\n</file>\n🦀\n";
        std::fs::write(&path, format!("\u{feff}{text}")).unwrap();
        let model = ModelRef {
            provider: "test".into(),
            model: "model".into(),
        };
        let prepare = |paths, images, limit| {
            prepare_files(
                paths,
                model.clone(),
                images,
                limit,
                CancellationToken::new(),
            )
        };
        let prepared = prepare(vec![path.clone()], false, 4096).await.unwrap();
        std::fs::write(&path, "changed after snapshot").unwrap();
        assert_eq!(prepared.text[0].text, text);
        let message = compose_prompt("  prompt 🦀  ".into(), &prepared.text, []);
        let Content::Text(body) = &message.content[0] else {
            panic!("text")
        };
        let json = body
            .strip_prefix("Attached file (UTF-8, JSON encoded):\n")
            .unwrap()
            .strip_suffix("\n  prompt 🦀  ")
            .unwrap();
        let data: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(data["text"], text);
        assert_eq!(data["name"], path.to_string_lossy().as_ref());
        assert!(prepare(vec![path.clone()], false, 4).await.is_err());
        for bytes in [b"\xff".as_slice(), b"binary\0data"] {
            std::fs::write(&path, bytes).unwrap();
            assert!(prepare(vec![path.clone()], false, 4096).await.is_err());
        }
        assert!(prepare(vec![root.clone()], false, 4096).await.is_err());
        assert!(
            prepare(vec![root.join("missing")], false, 4096)
                .await
                .is_err()
        );
        let image = root.join("picture.png");
        std::fs::write(&image, base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==").unwrap()).unwrap();
        assert!(prepare(vec![image.clone()], false, 4096).await.is_err());
        let prepared = prepare(vec![image], true, 4096).await.unwrap();
        assert_eq!(prepared.images.len(), 1);
        assert!(
            prepared.images[0]
                .note
                .as_deref()
                .unwrap()
                .contains("picture.png")
        );
        let stop = CancellationToken::new();
        stop.cancel();
        assert!(
            prepare_files(vec![path], model, true, 4096, stop)
                .await
                .is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
