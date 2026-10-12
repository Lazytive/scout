//! Scout's share protocol (M11, ADR 0021): what the self-hostable share server
//! (`apps/klip-share-server`) and the app agree on. Pure: types, ids, tokens and input
//! checks, no networking.
//!
//! The API, all JSON except the bodies noted:
//! - `GET /api/v1/info` (upload token) → [`ServerInfo`].
//! - `POST /api/v1/clips?title=…&expires_days=…` (upload token), body = the MP4 →
//!   201 [`Uploaded`].
//! - `PUT /api/v1/clips/{id}/poster` (the upload's manage token), body = a JPEG, PNG or
//!   WebP picture → 204.
//! - `DELETE /api/v1/clips/{id}` (manage token) → 204.
//! - Public: `GET /c/{id}` (player page with Open Graph tags), `/c/{id}.mp4` (Range),
//!   `/c/{id}.jpg|.png|.webp` (the poster).
//!
//! Errors are [`ApiError`] with an HTTP status. Tokens go in `Authorization: Bearer …`.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// `GET`: what the server takes.
pub const PATH_INFO: &str = "/api/v1/info";
/// `POST`: upload a clip; `/{id}` + `DELETE`, `/{id}/poster` + `PUT`.
pub const PATH_CLIPS: &str = "/api/v1/clips";
/// Public pages and files live under this.
pub const PATH_PUBLIC: &str = "/c";
/// Longest title kept (characters).
pub const MAX_TITLE_CHARS: usize = 120;
/// Largest poster picture.
pub const MAX_POSTER_BYTES: usize = 2 << 20;
/// Clip ids: 16 random bytes as lowercase hex.
pub const ID_LEN: usize = 32;
/// Upload tokens start with this (then 64 hex digits).
pub const UPLOAD_TOKEN_PREFIX: &str = "sst_";
/// Manage tokens (one per upload, for deleting it) start with this.
pub const MANAGE_TOKEN_PREFIX: &str = "ssm_";

/// `GET /api/v1/info`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    /// Always `scout-share`.
    pub server: String,
    pub version: String,
    pub max_upload_bytes: u64,
    pub quota_bytes: u64,
    pub used_bytes: u64,
    /// Longest expiry an upload may ask for, days (0: no limit, "never" allowed).
    pub max_expiry_days: u32,
    /// Expiry when an upload doesn't ask, days (0: never).
    pub default_expiry_days: u32,
    /// The name the upload token was made with.
    pub token_name: String,
}

/// `POST /api/v1/clips` answered 201.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Uploaded {
    pub id: String,
    /// The player page (what Scout copies).
    pub url: String,
    /// The MP4 itself.
    pub video_url: String,
    /// Deletes this upload and sets its poster; shown once, stored hashed.
    pub manage_token: String,
    /// 0: never.
    pub expires_unix: u64,
    pub bytes: u64,
}

/// Every error answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    /// Stable code: `unauthorized`, `too_big`, `quota`, `not_mp4`, `not_found`, …
    pub error: String,
    pub message: String,
}

/// `POST /api/v1/clips` query.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadQuery {
    pub title: Option<String>,
    /// Days until the link stops working (0: never, when the server allows it).
    pub expires_days: Option<u32>,
}

/// Lowercase hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn random_hex(n: usize) -> Result<String, getrandom::Error> {
    let mut b = vec![0u8; n];
    getrandom::fill(&mut b)?;
    Ok(hex(&b))
}

/// A new clip id (128 random bits).
pub fn new_id() -> Result<String, getrandom::Error> {
    random_hex(16)
}

/// A new upload token (256 random bits).
pub fn new_upload_token() -> Result<String, getrandom::Error> {
    Ok(format!("{UPLOAD_TOKEN_PREFIX}{}", random_hex(32)?))
}

/// A new manage token (256 random bits).
pub fn new_manage_token() -> Result<String, getrandom::Error> {
    Ok(format!("{MANAGE_TOKEN_PREFIX}{}", random_hex(32)?))
}

/// Whether `s` is a clip id (so it can name a folder safely).
pub fn valid_id(s: &str) -> bool {
    s.len() == ID_LEN && s.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

/// What the server stores for a token: hex SHA-256 (tokens are 256 random bits, so a
/// plain hash is enough).
pub fn hash_token(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

/// Equal strings, in time that doesn't depend on where they differ.
pub fn same_secret(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// A title safe to store and show: no control characters, trimmed, at most
/// [`MAX_TITLE_CHARS`] characters.
pub fn clean_title(s: &str) -> String {
    let t: String = s.chars().filter(|c| !c.is_control()).collect();
    t.trim().chars().take(MAX_TITLE_CHARS).collect()
}

/// A poster picture's file extension and media type, from its first bytes.
pub fn poster_kind(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(("jpg", "image/jpeg"))
    } else if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some(("png", "image/png"))
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP".as_slice()) {
        Some(("webp", "image/webp"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_tokens_have_their_shape() {
        let id = new_id().unwrap();
        assert!(valid_id(&id), "{id}");
        assert_ne!(id, new_id().unwrap());
        let t = new_upload_token().unwrap();
        assert!(t.starts_with("sst_") && t.len() == 4 + 64, "{t}");
        let m = new_manage_token().unwrap();
        assert!(m.starts_with("ssm_") && m.len() == 4 + 64, "{m}");
        for bad in [
            "",
            "../../etc/passwd",
            "0123456789abcdef0123456789abcdeF",
            "0123456789abcdef0123456789abcde",
            "0123456789abcdef0123456789abcdef0",
            "0123456789abcdef0123456789abcde.",
        ] {
            assert!(!valid_id(bad), "{bad}");
        }
        assert!(valid_id("0123456789abcdef0123456789abcdef"));
    }

    #[test]
    fn hashes_compare_without_shortcuts() {
        let h = hash_token("sst_x");
        assert_eq!(h.len(), 64);
        assert_eq!(
            hash_token("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(same_secret(&h, &hash_token("sst_x")));
        assert!(!same_secret(&h, &hash_token("sst_y")));
        assert!(!same_secret("ab", "abc"));
    }

    #[test]
    fn titles_are_cleaned_and_capped() {
        assert_eq!(clean_title("  Ace\u{0} round\n "), "Ace round");
        assert_eq!(
            clean_title(&"é".repeat(500)).chars().count(),
            MAX_TITLE_CHARS
        );
        assert_eq!(clean_title("\u{1}\u{2}"), "");
    }

    #[test]
    fn posters_are_known_by_their_first_bytes() {
        assert_eq!(
            poster_kind(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some(("jpg", "image/jpeg"))
        );
        assert_eq!(
            poster_kind(b"\x89PNG\r\n\x1a\nIHDR").map(|k| k.0),
            Some("png")
        );
        assert_eq!(
            poster_kind(b"RIFF\0\0\0\0WEBPVP8 ").map(|k| k.0),
            Some("webp")
        );
        assert_eq!(poster_kind(b"RIFF\0\0\0\0WAVE"), None);
        assert_eq!(poster_kind(b"<svg"), None);
        assert_eq!(poster_kind(b""), None);
    }

    #[test]
    fn messages_round_trip_as_json() {
        let u = Uploaded {
            id: "0123456789abcdef0123456789abcdef".into(),
            url: "https://clips.example/c/0123456789abcdef0123456789abcdef".into(),
            video_url: "https://clips.example/c/0123456789abcdef0123456789abcdef.mp4".into(),
            manage_token: "ssm_1".into(),
            expires_unix: 0,
            bytes: 9,
        };
        let back: Uploaded = serde_json::from_str(&serde_json::to_string(&u).unwrap()).unwrap();
        assert_eq!(back, u);
    }
}
