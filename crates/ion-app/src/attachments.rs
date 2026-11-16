//! Explicit user attachments: bounded, owned snapshots before Core admission.
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use base64::{display::Base64Display, engine::general_purpose::STANDARD};
use ion_ai::{Content, ImageMime, LoadedImage, MAX_SOURCE_BYTES, Message, ModelRef};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

/// Sources belong to application input custody, never the durable model message.
/// File paths are literal; clients own human syntax such as `~` expansion.
#[derive(Clone, Debug, Serialize)]
pub enum AttachmentSource {
    /// Detect a supported image or UTF-8 text from the file contents.
    File(PathBuf),
    /// Require an image rather than accepting a text file.
    ImageFile(PathBuf),
    ImageBytes {
        #[serde(serialize_with = "serialize_image_bytes")]
        bytes: Vec<u8>,
        /// An assertion about the bytes, not an override of content detection.
        mime_type: Option<ImageMime>,
    },
    Image(LoadedImage),
    Text(TextFile),
}

impl AttachmentSource {
    pub fn image_bytes(bytes: Vec<u8>) -> Self {
        Self::ImageBytes {
            bytes,
            mime_type: None,
        }
    }

    pub(crate) fn path(&self) -> Option<&std::path::Path> {
        match self {
            Self::File(path) | Self::ImageFile(path) => Some(path),
            _ => None,
        }
    }

    pub(crate) fn is_image(&self) -> bool {
        matches!(
            self,
            Self::ImageFile(_) | Self::ImageBytes { .. } | Self::Image(_)
        )
    }
}

impl From<LoadedImage> for AttachmentSource {
    fn from(image: LoadedImage) -> Self {
        Self::Image(image)
    }
}

fn serialize_image_bytes<S: serde::Serializer>(
    bytes: &[u8],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_str(&Base64Display::new(bytes, &STANDARD))
}

/// Literal UTF-8 context. Once committed this is message text, not a live file.
#[derive(Clone, Debug, Serialize)]
pub struct TextFile {
    pub name: String,
    pub text: String,
}

impl TextFile {
    fn model_text(&self) -> String {
        // Framing keeps names and apparent delimiters inside a data envelope;
        // it is not a prompt-injection guarantee.
        format!(
            "Attached file (UTF-8, JSON encoded):\n{}\n",
            serde_json::to_string(self).expect("strings serialize")
        )
    }
}

/// Expand resource commands in the prompt before composition, never in files.
pub fn compose_prompt(
    text: String,
    files: &[TextFile],
    images: impl IntoIterator<Item = LoadedImage>,
) -> Message {
    let mut composed = String::new();
    for file in files {
        composed.push_str(&file.model_text());
    }
    composed.push_str(&text);
    Message::user_input(composed, images)
}

#[derive(Default)]
pub struct PreparedAttachments {
    pub text: Vec<TextFile>,
    pub images: Vec<LoadedImage>,
}

impl PreparedAttachments {
    pub fn into_sources(self) -> Vec<AttachmentSource> {
        self.text
            .into_iter()
            .map(AttachmentSource::Text)
            .chain(self.images.into_iter().map(AttachmentSource::Image))
            .collect()
    }
}

/// One aggregate bound for retained inline sources, before cloning or dispatch.
/// Final normalized payload and Core request bounds are checked separately.
pub(crate) fn validate_sources(sources: &[AttachmentSource], limit: usize) -> Result<()> {
    ensure!(
        sources.len() <= limit / br#"{"Text":""}"#.len(),
        "attachment batch exceeds input byte limit"
    );
    let mut bytes = 0usize;
    for source in sources {
        let size = match source {
            AttachmentSource::ImageBytes { bytes, .. } => bytes.len(),
            AttachmentSource::Image(image) => image.content.data().len(),
            AttachmentSource::Text(file) => file
                .text
                .len()
                .checked_add(file.name.len())
                .context("attachment source size overflow")?,
            AttachmentSource::File(_) | AttachmentSource::ImageFile(_) => 0,
        };
        bytes = bytes
            .checked_add(size)
            .context("attachment source size overflow")?;
    }
    ensure!(
        bytes <= MAX_SOURCE_BYTES,
        "attachment batch exceeds 32 MiB source bound"
    );
    Ok(())
}

/// Always join the blocking worker. Cancellation prevents queued reads and
/// discards completed results, but cannot roll back an active read or decoder.
/// Any failure rejects the entire batch, never a partially prepared prompt.
/// File sources must be absolute: calling this helper authorizes their read.
/// `SessionControl` checks application authority and resolves Session-relative
/// sources before calling it; this helper never uses an ambient working directory.
pub fn prepare_attachments(
    model: &ModelRef,
    image_input: bool,
    sources: Vec<AttachmentSource>,
    max_payload_bytes: usize,
    stop: CancellationToken,
) -> impl std::future::Future<Output = Result<PreparedAttachments>> + Send + use<> {
    let model = model.clone();
    async move {
        ensure!(!stop.is_cancelled(), "attachment preparation cancelled");
        validate_sources(&sources, max_payload_bytes)?;
        ensure!(
            sources
                .iter()
                .all(|source| source.path().is_none_or(std::path::Path::is_absolute)),
            "attachment preparation requires absolute file paths; use SessionControl for Session-relative input"
        );
        if sources.is_empty() {
            return Ok(PreparedAttachments::default());
        }
        if sources.iter().any(AttachmentSource::is_image) {
            require_image_input(&model, image_input)?;
        }
        let worker_stop = stop.clone();
        let joined = tokio::task::spawn_blocking(move || {
            let mut prepared = PreparedAttachments::default();
            let mut payload = 0usize;
            for source in sources {
                ensure!(
                    !worker_stop.is_cancelled(),
                    "attachment preparation cancelled"
                );
                let source = load_source(source, &model, image_input, &worker_stop)?;
                let size = match source {
                    PreparedAttachment::Text(file) => {
                        let size = serde_json::to_vec(&Content::Text(file.model_text()))?.len();
                        prepared.text.push(file);
                        size
                    }
                    PreparedAttachment::Image(image) => {
                        let size = serde_json::to_vec(&image)?.len();
                        prepared.images.push(image);
                        size
                    }
                };
                payload = payload
                    .checked_add(size)
                    .context("attachment payload size overflow")?;
                ensure!(
                    payload <= max_payload_bytes,
                    "attachment batch exceeds input byte limit"
                );
            }
            Ok(prepared)
        })
        .await;
        let result = match joined {
            Ok(result) => result?,
            Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            Err(error) => return Err(error).context("attachment preparation worker stopped"),
        };
        ensure!(!stop.is_cancelled(), "attachment preparation cancelled");
        Ok(result)
    }
}

enum PreparedAttachment {
    Text(TextFile),
    Image(LoadedImage),
}

fn load_source(
    source: AttachmentSource,
    model: &ModelRef,
    image_input: bool,
    stop: &CancellationToken,
) -> Result<PreparedAttachment> {
    let image_only = matches!(&source, AttachmentSource::ImageFile(_));
    match source {
        AttachmentSource::Image(image) => Ok(PreparedAttachment::Image(image)),
        AttachmentSource::Text(file) => Ok(PreparedAttachment::Text(file)),
        AttachmentSource::ImageBytes { bytes, mime_type } => {
            if let Some(declared) = mime_type {
                ensure!(
                    ImageMime::detect(&bytes) == Some(declared),
                    "image data does not match its MIME type"
                );
            }
            Ok(PreparedAttachment::Image(
                ion_ai::normalize_image(&bytes).context("invalid inline image")?,
            ))
        }
        AttachmentSource::File(path) | AttachmentSource::ImageFile(path) => {
            let bytes =
                crate::file_io::read_bounded(&path, MAX_SOURCE_BYTES).with_context(|| {
                    format!("cannot read attachment {path:?} (regular file, 32 MiB maximum)")
                })?;
            ensure!(!stop.is_cancelled(), "attachment preparation cancelled");
            if image_only || ImageMime::detect(&bytes).is_some() {
                require_image_input(model, image_input)?;
                let mut image = ion_ai::normalize_image(&bytes)
                    .with_context(|| format!("invalid image {path:?}"))?;
                if let Some(name) = path.to_str() {
                    let mut note = format!("Attached image file: {}", serde_json::to_string(name)?);
                    if let Some(normalization) = image.note.take() {
                        note.push('\n');
                        note.push_str(&normalization);
                    }
                    image.note = Some(note);
                }
                Ok(PreparedAttachment::Image(image))
            } else {
                let name = path
                    .to_str()
                    .context("text attachment path is not UTF-8")?
                    .to_owned();
                let text = String::from_utf8(bytes).with_context(|| {
                    format!("attachment {path:?} is not UTF-8 text or a supported image")
                })?;
                ensure!(
                    !text.contains('\0'),
                    "attachment {path:?} contains binary NUL bytes"
                );
                let text = text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned();
                Ok(PreparedAttachment::Text(TextFile { name, text }))
            }
        }
    }
}

pub fn require_image_input(model: &ModelRef, image_input: bool) -> Result<()> {
    ensure!(
        image_input,
        "{}/{} does not declare image input; choose an image-capable model or configure the custom route with --images",
        model.provider,
        model.model
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    fn model() -> ModelRef {
        ModelRef {
            provider: "synthetic".into(),
            model: "vision".into(),
        }
    }

    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";

    #[tokio::test]
    async fn direct_preparation_rejects_ambient_relative_paths() {
        for source in [
            AttachmentSource::File("Cargo.toml".into()),
            AttachmentSource::ImageFile("~/picture.png".into()),
        ] {
            let error =
                prepare_attachments(&model(), true, vec![source], 4096, CancellationToken::new())
                    .await
                    .err()
                    .unwrap();
            assert!(
                error.to_string().contains("absolute file paths"),
                "{error:#}"
            );
        }
    }

    #[tokio::test]
    async fn mixed_sources_share_a_payload_bound_and_explicit_images_reject_text() {
        let root = std::env::temp_dir().join(format!("ion-attachments-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let text = root.join("literal.md");
        let picture = root.join("picture.png");
        std::fs::write(&text, "/not-a-command {{ARG}}").unwrap();
        std::fs::write(&picture, STANDARD.decode(PNG).unwrap()).unwrap();
        let sources = || {
            vec![
                AttachmentSource::File(text.clone()),
                AttachmentSource::File(picture.clone()),
            ]
        };
        let prepared =
            prepare_attachments(&model(), true, sources(), 4096, CancellationToken::new())
                .await
                .unwrap();
        assert_eq!(prepared.text[0].text, "/not-a-command {{ARG}}");
        assert_eq!(prepared.images.len(), 1);
        let file_bytes = serde_json::to_vec(&Content::Text(prepared.text[0].model_text()))
            .unwrap()
            .len();
        let image_bytes = serde_json::to_vec(&prepared.images[0]).unwrap().len();
        let limit = file_bytes.max(image_bytes);
        for source in sources() {
            assert!(
                prepare_attachments(
                    &model(),
                    true,
                    vec![source],
                    limit,
                    CancellationToken::new()
                )
                .await
                .is_ok()
            );
        }
        let error = prepare_attachments(&model(), true, sources(), limit, CancellationToken::new())
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("input byte limit"), "{error:#}");
        assert!(
            prepare_attachments(
                &model(),
                true,
                vec![AttachmentSource::ImageFile(text.clone())],
                4096,
                CancellationToken::new()
            )
            .await
            .is_err()
        );
        let text_only = prepare_attachments(
            &model(),
            false,
            vec![AttachmentSource::File(text.clone())],
            4096,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(text_only.text[0].text, "/not-a-command {{ARG}}");
        assert!(
            prepare_attachments(&model(), false, sources(), 4096, CancellationToken::new())
                .await
                .is_err()
        );
        std::fs::remove_file(&text).unwrap();
        std::fs::remove_file(&picture).unwrap();
        let message = compose_prompt("prompt".into(), &prepared.text, prepared.images);
        assert!(
            matches!(&message.content[0], Content::Text(text) if text.contains("/not-a-command {{ARG}}"))
        );
        assert!(
            message
                .content
                .iter()
                .any(|part| matches!(part, Content::Image(_)))
        );
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn bytes_have_a_bounded_base64_custody_representation() {
        let source = AttachmentSource::image_bytes(vec![1, 2, 3]);
        assert_eq!(
            serde_json::to_value(source).unwrap(),
            serde_json::json!({"ImageBytes":{"bytes":"AQID","mime_type":null}})
        );
        let oversized = AttachmentSource::image_bytes(vec![0; MAX_SOURCE_BYTES + 1]);
        assert!(
            validate_sources(&[oversized], usize::MAX)
                .unwrap_err()
                .to_string()
                .contains("32 MiB")
        );
    }

    #[test]
    fn queued_preparation_keeps_polling_live_and_cancels_before_read() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let (entered, waiting) = tokio::sync::oneshot::channel();
            let (release, held) = tokio::sync::oneshot::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                entered.send(()).unwrap();
                held.blocking_recv().unwrap();
            });
            waiting.await.unwrap();
            let stop = CancellationToken::new();
            let preparation = tokio::spawn(prepare_attachments(
                &model(),
                true,
                vec![
                    AttachmentSource::File(std::env::temp_dir().join("missing-file")),
                    AttachmentSource::ImageFile(std::env::temp_dir().join("missing-image")),
                ],
                1024,
                stop.clone(),
            ));
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            let pending = !preparation.is_finished();
            stop.cancel();
            release.send(()).unwrap();
            blocker.await.unwrap();
            let error = preparation.await.unwrap().err().unwrap();
            assert!(pending, "image preparation bypassed blocking dispatch");
            assert!(error.to_string().contains("cancelled"), "{error:#}");
        });
    }

    #[tokio::test]
    async fn batch_validation_is_atomic_and_bounds_retained_payload() {
        let source = || AttachmentSource::ImageBytes {
            mime_type: Some(ImageMime::Png),
            bytes: STANDARD.decode(PNG).unwrap(),
        };
        let image = prepare_attachments(
            &model(),
            true,
            vec![source()],
            1024,
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .images
        .pop()
        .unwrap();
        let bytes = serde_json::to_vec(&image).unwrap().len();
        for (sources, expected) in [
            (vec![source(), source()], "input byte limit"),
            (
                vec![
                    source(),
                    AttachmentSource::ImageBytes {
                        mime_type: Some(ImageMime::Jpeg),
                        bytes: STANDARD.decode(PNG).unwrap(),
                    },
                ],
                "MIME type",
            ),
        ] {
            let error =
                prepare_attachments(&model(), true, sources, bytes, CancellationToken::new())
                    .await
                    .err()
                    .unwrap();
            assert!(format!("{error:#}").contains(expected), "{error:#}");
        }
    }
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
        let prepare = |paths: Vec<PathBuf>, images, limit| {
            prepare_attachments(
                &model,
                images,
                paths.into_iter().map(AttachmentSource::File).collect(),
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
            prepare_attachments(&model, true, vec![AttachmentSource::File(path)], 4096, stop)
                .await
                .is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
