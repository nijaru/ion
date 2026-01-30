//! PKCE (Proof Key for Code Exchange) implementation.
//!
//! RFC 7636: <https://datatracker.ietf.org/doc/html/rfc7636>

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::Rng;
use sha2::{Digest, Sha256};

/// PKCE code verifier and challenge pair.
#[derive(Debug, Clone)]
pub struct PkceCodes {
    /// The code verifier (43-128 chars, base64url).
    pub verifier: String,
    /// The code challenge (SHA-256 hash of verifier, base64url).
    pub challenge: String,
}

impl PkceCodes {
    /// Generate a new PKCE code pair.
    #[must_use]
    pub fn generate() -> Self {
        // Generate 32 random bytes for verifier
        let mut verifier_bytes = [0u8; 32];
        rand::rng().fill(&mut verifier_bytes);

        // Base64url encode (no padding) for verifier
        let verifier = URL_SAFE_NO_PAD.encode(verifier_bytes);

        // SHA-256 hash the verifier, then base64url encode for challenge
        let mut hasher = Sha256::new();
        hasher.update(verifier.as_bytes());
        let challenge = URL_SAFE_NO_PAD.encode(hasher.finalize());

        Self {
            verifier,
            challenge,
        }
    }
}

/// Generate a random state string for CSRF protection.
#[must_use]
pub fn generate_state() -> String {
    let mut state_bytes = [0u8; 32];
    rand::rng().fill(&mut state_bytes);
    URL_SAFE_NO_PAD.encode(state_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pkce_generation() {
        let codes = PkceCodes::generate();

        // Verifier should be 43 chars (32 bytes base64url encoded)
        assert_eq!(codes.verifier.len(), 43);

        // Challenge should be 43 chars (32 bytes SHA-256 hash base64url encoded)
        assert_eq!(codes.challenge.len(), 43);

        // Each generation should be unique
        let codes2 = PkceCodes::generate();
        assert_ne!(codes.verifier, codes2.verifier);
    }

    #[test]
    fn test_state_generation() {
        let state = generate_state();
        assert_eq!(state.len(), 43);

        let state2 = generate_state();
        assert_ne!(state, state2);
    }
}
