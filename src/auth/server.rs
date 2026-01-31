//! Local callback server for OAuth redirects.

use anyhow::{Result, anyhow, bail};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use url::Url;

/// Default callback port (same as Codex CLI).
pub const DEFAULT_PORT: u16 = 1455;

/// Result from OAuth callback.
#[derive(Debug, Clone)]
pub struct CallbackResult {
    /// Authorization code from the OAuth provider.
    pub code: String,
    /// State parameter for validation.
    pub state: String,
}

/// Local HTTP server for OAuth callbacks.
pub struct CallbackServer {
    port: u16,
    expected_state: String,
    listener: TcpListener,
}

impl CallbackServer {
    /// Create a new callback server.
    ///
    /// Tries the default port first, then finds an available port.
    pub fn new(expected_state: String) -> Result<Self> {
        // Try default port first
        let listener = match TcpListener::bind(format!("127.0.0.1:{DEFAULT_PORT}")) {
            Ok(l) => l,
            Err(_) => {
                // Find an available port
                TcpListener::bind("127.0.0.1:0")?
            }
        };

        let port = listener.local_addr()?.port();

        // Set non-blocking for timeout support
        listener.set_nonblocking(true)?;

        Ok(Self {
            port,
            expected_state,
            listener,
        })
    }

    /// Get the port the server is listening on.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Get the redirect URI for OAuth.
    #[must_use]
    pub fn redirect_uri(&self) -> String {
        format!("http://localhost:{}/auth/callback", self.port)
    }

    /// Wait for the OAuth callback with timeout.
    pub fn wait_for_callback(self, timeout: Duration) -> Result<CallbackResult> {
        let (tx, rx) = mpsc::channel();
        let expected_state = self.expected_state.clone();

        // Spawn a thread to handle the request
        let listener = self.listener;
        thread::spawn(move || {
            let result = Self::handle_requests(listener, &expected_state, timeout);
            let _ = tx.send(result);
        });

        // Wait for result with timeout
        rx.recv_timeout(timeout)
            .map_err(|_| anyhow!("OAuth callback timeout"))?
    }

    #[allow(clippy::needless_pass_by_value)]
    fn handle_requests(
        listener: TcpListener,
        expected_state: &str,
        timeout: Duration,
    ) -> Result<CallbackResult> {
        let start = std::time::Instant::now();

        loop {
            // Check timeout
            if start.elapsed() > timeout {
                bail!("OAuth callback timeout");
            }

            // Try to accept a connection
            match listener.accept() {
                Ok((mut stream, _)) => {
                    // Read the request
                    let mut buffer = [0u8; 4096];
                    stream.set_read_timeout(Some(Duration::from_secs(5)))?;

                    let Ok(n) = stream.read(&mut buffer) else {
                        continue;
                    };

                    let request = String::from_utf8_lossy(&buffer[..n]);

                    // Parse the request line
                    let Some(path) = request.lines().next().and_then(|line| {
                        let parts: Vec<_> = line.split_whitespace().collect();
                        if parts.len() >= 2 && parts[0] == "GET" {
                            Some(parts[1])
                        } else {
                            None
                        }
                    }) else {
                        continue;
                    };

                    // Handle callback path
                    if path.starts_with("/auth/callback") {
                        match Self::parse_callback(path, expected_state) {
                            Ok(result) => {
                                // Send success response
                                Self::send_success_response(&mut stream)?;
                                return Ok(result);
                            }
                            Err(e) => {
                                // Send error response
                                Self::send_error_response(&mut stream, &e.to_string())?;
                                bail!(e);
                            }
                        }
                    }
                    // Send 404 for other paths
                    Self::send_404_response(&mut stream)?;
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // No connection yet, wait a bit
                    thread::sleep(Duration::from_millis(100));
                }
                Err(e) => {
                    bail!("Failed to accept connection: {e}");
                }
            }
        }
    }

    fn parse_callback(path: &str, expected_state: &str) -> Result<CallbackResult> {
        // Parse as URL to extract query params
        let url = Url::parse(&format!("http://localhost{path}"))?;
        let params: HashMap<_, _> = url.query_pairs().collect();

        // Check for error
        if let Some(error) = params.get("error") {
            let description = params
                .get("error_description")
                .map(std::string::ToString::to_string)
                .unwrap_or_default();
            bail!("OAuth error: {error} - {description}");
        }

        // Get and validate state
        let state = params
            .get("state")
            .ok_or_else(|| anyhow!("Missing state parameter"))?;

        if state.as_ref() != expected_state {
            bail!("State mismatch - possible CSRF attack");
        }

        // Get authorization code
        let code = params
            .get("code")
            .ok_or_else(|| anyhow!("Missing authorization code"))?;

        Ok(CallbackResult {
            code: code.to_string(),
            state: state.to_string(),
        })
    }

    fn send_success_response(stream: &mut std::net::TcpStream) -> Result<()> {
        let body = r"<!DOCTYPE html>
<html>
<head>
    <title>Login Successful</title>
    <style>
        body { font-family: system-ui, sans-serif; text-align: center; padding: 50px; }
        h1 { color: #22c55e; }
    </style>
</head>
<body>
    <h1>Login Successful!</h1>
    <p>You can close this tab and return to ion.</p>
</body>
</html>";

        let response = format!(
            "HTTP/1.1 200 OK\r\n\
             Content-Type: text/html\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             \r\n\
             {}",
            body.len(),
            body
        );

        stream.write_all(response.as_bytes())?;
        stream.flush()?;
        Ok(())
    }

    fn send_error_response(stream: &mut std::net::TcpStream, error: &str) -> Result<()> {
        let body = format!(
            r"<!DOCTYPE html>
<html>
<head>
    <title>Login Failed</title>
    <style>
        body {{ font-family: system-ui, sans-serif; text-align: center; padding: 50px; }}
        h1 {{ color: #ef4444; }}
    </style>
</head>
<body>
    <h1>Login Failed</h1>
    <p>{}</p>
    <p>Please try again.</p>
</body>
</html>",
            html_escape(error)
        );

        let response = format!(
            "HTTP/1.1 400 Bad Request\r\n\
             Content-Type: text/html\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             \r\n\
             {}",
            body.len(),
            body
        );

        stream.write_all(response.as_bytes())?;
        stream.flush()?;
        Ok(())
    }

    fn send_404_response(stream: &mut std::net::TcpStream) -> Result<()> {
        let response = "HTTP/1.1 404 Not Found\r\n\
                        Content-Length: 0\r\n\
                        Connection: close\r\n\
                        \r\n";

        stream.write_all(response.as_bytes())?;
        stream.flush()?;
        Ok(())
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_callback_success() {
        let path = "/auth/callback?code=abc123&state=xyz789";
        let result = CallbackServer::parse_callback(path, "xyz789").unwrap();
        assert_eq!(result.code, "abc123");
        assert_eq!(result.state, "xyz789");
    }

    #[test]
    fn test_parse_callback_state_mismatch() {
        let path = "/auth/callback?code=abc123&state=wrong";
        let result = CallbackServer::parse_callback(path, "expected");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("State mismatch"));
    }

    #[test]
    fn test_parse_callback_error() {
        let path = "/auth/callback?error=access_denied&error_description=User%20denied%20access";
        let result = CallbackServer::parse_callback(path, "any");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("access_denied"));
    }
}
