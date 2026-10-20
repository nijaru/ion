//! Bounded text projection. HTML parsing/rendering and snapshot I/O stay in the
//! joined blocking worker, not in the network future that cancellation drops.
use std::{
    fs::OpenOptions,
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
};
use tokio_util::sync::CancellationToken;
use url::Url;

pub(super) type Failure = (&'static str, &'static str);

pub(super) struct Projection {
    pub text: String,
    pub format: &'static str,
    pub bytes: usize,
    pub truncated: bool,
    pub full_path: Option<String>,
    pub full_error: Option<String>,
}

pub(super) fn media_type(header: Option<&str>) -> Result<String, Failure> {
    let raw = header.filter(|value| value.len() <= 256).ok_or((
        "unsupported_content",
        "Response requires an explicit supported textual Content-Type.",
    ))?;
    let mut parts = raw.split(';');
    let media = parts
        .next()
        .expect("split always yields a value")
        .trim()
        .to_ascii_lowercase();
    if !(media.starts_with("text/")
        || matches!(
            media.as_str(),
            "application/json"
                | "application/xml"
                | "application/xhtml+xml"
                | "application/javascript"
                | "application/yaml"
        )
        || media.ends_with("+json")
        || media.ends_with("+xml"))
    {
        return Err((
            "unsupported_content",
            "Only UTF-8 text, JSON, XML and HTML are supported; PDF/images/binary are not decoded.",
        ));
    }
    for part in parts {
        if let Some((name, value)) = part.trim().split_once('=')
            && name.trim().eq_ignore_ascii_case("charset")
            && !matches!(
                value.trim().trim_matches('"').to_ascii_lowercase().as_str(),
                "utf-8" | "utf8" | "us-ascii"
            )
        {
            return Err((
                "unsupported_charset",
                "Only UTF-8/ASCII responses are supported.",
            ));
        }
    }
    Ok(media)
}

pub(super) fn check(
    stop: &CancellationToken,
    deadline: tokio::time::Instant,
) -> Result<(), Failure> {
    if stop.is_cancelled() {
        return Err(("cancelled", "Text conversion stopped."));
    }
    if tokio::time::Instant::now() >= deadline {
        return Err((
            "timeout",
            "Text conversion exceeded the retrieval deadline.",
        ));
    }
    Ok(())
}

pub(super) fn project(
    body: Vec<u8>,
    media: &str,
    max_bytes: usize,
    final_url: Url,
    stop: CancellationToken,
    deadline: tokio::time::Instant,
) -> Result<Projection, Failure> {
    check(&stop, deadline)?;
    let source = String::from_utf8(body).map_err(|_| {
        (
            "unsupported_charset",
            "Response is not valid UTF-8; no lossy decoding was performed.",
        )
    })?;
    let source = source.strip_prefix('\u{feff}').unwrap_or(&source);
    if source.chars().any(unsafe_control) {
        return Err((
            "unsupported_content",
            "Response contains non-text control characters.",
        ));
    }
    let html = matches!(media, "text/html" | "application/xhtml+xml");
    let mut text = if html {
        super::html::extract(source, final_url, &stop, deadline)?
    } else {
        source.to_owned()
    };
    check(&stop, deadline)?;
    if text.chars().any(unsafe_control) {
        return Err((
            "unsupported_content",
            "Converted content contains non-text control characters.",
        ));
    }
    let bytes = text.len();
    let truncated = bytes > max_bytes;
    let (full_path, full_error) = if truncated {
        match save_snapshot(&text) {
            Ok(path) => (Some(path), None),
            Err(error) => (
                None,
                Some(format!(
                    "Cannot save full converted text: {:?}",
                    error.kind()
                )),
            ),
        }
    } else {
        (None, None)
    };
    let mut end = text.len().min(max_bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    Ok(Projection {
        text,
        format: if html { "html_text" } else { "text" },
        bytes,
        truncated,
        full_path,
        full_error,
    })
}

fn unsafe_control(c: char) -> bool {
    c.is_control() && !matches!(c, '\n' | '\r' | '\t')
}

fn save_snapshot(text: &str) -> io::Result<String> {
    let path = std::env::temp_dir().join(format!("ion-web-{}.txt", uuid::Uuid::now_v7()));
    let display = path
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "snapshot path is not UTF-8"))?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    if let Err(error) = file.write_all(text.as_bytes()).and_then(|()| file.flush()) {
        let _ = std::fs::remove_file(&path);
        return Err(error);
    }
    Ok(display.to_owned())
}
