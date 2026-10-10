//! Owned user-image preparation. Capability and filesystem access belong here;
//! normalization is shared with images returned by coding tools.
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use ion_ai::{ImageMime, MAX_SOURCE_BYTES, normalize_image};
use tokio_util::sync::CancellationToken;

use crate::Selection;

pub use ion_ai::LoadedImage;

pub enum ImageSource {
    Path(PathBuf),
    Encoded { mime_type: String, data: String },
}

/// Capture the route and sources, then own and join the blocking work. Cancelling
/// stops queued work and discards results, but cannot interrupt an active decoder.
/// This payload bound supplements, not replaces, Core's final input admission.
pub fn prepare_images(
    selected: &Selection,
    sources: Vec<ImageSource>,
    max_payload_bytes: usize,
    stop: CancellationToken,
) -> impl std::future::Future<Output = Result<Vec<LoadedImage>>> + Send + use<> {
    let capability = if sources.is_empty() {
        Ok(())
    } else {
        require_image_input(selected)
    };
    async move {
        capability?;
        ensure!(!stop.is_cancelled(), "image preparation cancelled");
        if sources.is_empty() {
            return Ok(Vec::new());
        }
        // Even an empty encoded image requires this JSON envelope. Reject batches
        // that cannot fit before retaining a worker or accumulating attachments.
        let envelope_bytes = br#"{"mime_type":"image/gif","data":""}"#.len();
        ensure!(
            sources.len() <= max_payload_bytes / envelope_bytes,
            "image batch exceeds input byte limit"
        );
        let retained_inline_bytes = sources
            .iter()
            .try_fold(0usize, |bytes, source| {
                bytes.checked_add(match source {
                    ImageSource::Encoded { data, .. } => data.len(),
                    ImageSource::Path(_) => 0,
                })
            })
            .context("inline image batch size overflow")?;
        ensure!(
            retained_inline_bytes <= MAX_SOURCE_BYTES.div_ceil(3) * 4,
            "inline image batch exceeds 32 MiB source bound"
        );
        let worker_stop = stop.clone();
        let joined = tokio::task::spawn_blocking(move || {
            let mut images = Vec::new();
            let mut payload_bytes = 0usize;
            for source in sources {
                ensure!(!worker_stop.is_cancelled(), "image preparation cancelled");
                let image = load_source(source)?;
                payload_bytes = payload_bytes
                    .checked_add(serde_json::to_vec(&image.content)?.len())
                    .and_then(|bytes| bytes.checked_add(image.note.as_ref().map_or(0, String::len)))
                    .context("image payload size overflow")?;
                ensure!(
                    payload_bytes <= max_payload_bytes,
                    "image batch exceeds input byte limit"
                );
                images.push(image);
            }
            Ok(images)
        })
        .await;
        let images = match joined {
            Ok(result) => result?,
            Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            Err(error) => return Err(error).context("image preparation worker stopped"),
        };
        ensure!(!stop.is_cancelled(), "image preparation cancelled");
        Ok(images)
    }
}

fn load_source(source: ImageSource) -> Result<LoadedImage> {
    match source {
        ImageSource::Path(path) => {
            let bytes =
                crate::file_io::read_bounded(&path, MAX_SOURCE_BYTES).with_context(|| {
                    format!(
                        "cannot read image {} (regular file, 32 MiB maximum)",
                        path.display()
                    )
                })?;
            normalize_image(&bytes).with_context(|| format!("invalid image {}", path.display()))
        }
        ImageSource::Encoded { mime_type, data } => {
            let declared = ImageMime::parse(&mime_type)
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
    }
}

/// Validate input capability without preparing already-normalized image bytes.
pub fn require_image_input(selected: &Selection) -> Result<()> {
    ensure!(
        selected.image_input,
        "{}/{} does not declare image input; choose an image-capable model or configure the custom route with --images",
        selected.provider,
        selected.model
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selected() -> Selection {
        Selection {
            provider: "local".into(),
            model: "vision".into(),
            endpoint: "http://127.0.0.1:1".into(),
            wire: crate::HttpWire::ChatCompletions,
            api_key_env: None,
            max_output_tokens: 1024,
            context_window_tokens: None,
            requires_key: false,
            image_input: true,
            capabilities: crate::catalog::ModelCapabilities::conservative(),
        }
    }

    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";

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
            let preparation = tokio::spawn(prepare_images(
                &selected(),
                vec![ImageSource::Path(PathBuf::from("missing-image"))],
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
        let source = || ImageSource::Encoded {
            mime_type: "image/png".into(),
            data: PNG.into(),
        };
        let image = prepare_images(&selected(), vec![source()], 1024, CancellationToken::new())
            .await
            .unwrap()
            .pop()
            .unwrap();
        let bytes = serde_json::to_vec(&image.content).unwrap().len();
        for (sources, expected) in [
            (vec![source(), source()], "input byte limit"),
            (
                vec![
                    source(),
                    ImageSource::Encoded {
                        mime_type: "image/jpeg".into(),
                        data: PNG.into(),
                    },
                ],
                "MIME type",
            ),
        ] {
            let error = prepare_images(&selected(), sources, bytes, CancellationToken::new())
                .await
                .err()
                .unwrap();
            assert!(format!("{error:#}").contains(expected), "{error:#}");
        }
    }
}
