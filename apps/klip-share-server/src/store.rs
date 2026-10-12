//! What the server keeps on disk, under `data_dir`:
//! - `clips/<id>/video.mp4`, `meta.json`, `poster.<jpg|png|webp>`;
//! - `tokens.json`: upload tokens, hashed;
//! - `tmp/`: uploads being received (emptied at start).
//!
//! Every write is a temporary file renamed into place. Ids are checked before they name
//! a folder. The space all clips take is counted at start and kept in step.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use klip_share::{hash_token, same_secret, valid_id};
use serde::{Deserialize, Serialize};

/// Largest `meta.json` read.
const MAX_META_BYTES: u64 = 64 << 10;
/// Largest `tokens.json` read.
const MAX_TOKENS_BYTES: u64 = 1 << 20;
/// Most upload tokens kept.
pub const MAX_TOKENS: usize = 1000;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Bad(String),
}

/// `clips/<id>/meta.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    pub id: String,
    pub title: String,
    pub created_unix: u64,
    /// 0: never.
    pub expires_unix: u64,
    pub bytes: u64,
    pub width: u32,
    pub height: u32,
    pub duration_ms: u64,
    pub codec: String,
    /// The poster's extension, when there is one.
    pub poster: Option<String>,
    /// Hash of the upload's manage token.
    pub manage_hash: String,
    /// The upload token's name.
    pub uploader: String,
}

impl Meta {
    pub fn expired(&self, now_unix: u64) -> bool {
        self.expires_unix != 0 && now_unix >= self.expires_unix
    }
}

/// One upload token (`tokens.json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TokenEntry {
    pub name: String,
    pub hash: String,
    pub created_unix: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TokenFile {
    pub tokens: Vec<TokenEntry>,
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Write `bytes` to `path` through a temporary file beside it.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let dir = path
        .parent()
        .ok_or_else(|| StoreError::Bad("no folder".into()))?;
    let name = path
        .file_name()
        .ok_or_else(|| StoreError::Bad("no file name".into()))?;
    let mut tmp_name = name.to_os_string();
    tmp_name.push(format!(".{}.tmp", random_suffix()?));
    let tmp = dir.join(tmp_name);
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    Ok(result?)
}

fn random_suffix() -> Result<String, StoreError> {
    klip_share::new_id().map_err(|e| StoreError::Bad(format!("no randomness: {e}")))
}

fn read_capped(path: &Path, cap: u64) -> Result<Option<Vec<u8>>, StoreError> {
    match std::fs::metadata(path) {
        Ok(m) if m.len() > cap => Err(StoreError::Bad(format!("{} is too big", path.display()))),
        Ok(_) => Ok(Some(std::fs::read(path)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// A token name: 1–40 of `A-Z a-z 0-9 space . _ -`.
pub fn valid_token_name(name: &str) -> bool {
    (1..=40).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-'))
        && name.trim() == name
}

pub struct Store {
    data: PathBuf,
    used: Mutex<u64>,
}

impl Store {
    /// Make the folders, empty `tmp/`, count the clips' size.
    pub fn open(data: &Path) -> Result<Store, StoreError> {
        let store = Store {
            data: data.to_path_buf(),
            used: Mutex::new(0),
        };
        std::fs::create_dir_all(store.clips_dir())?;
        let tmp = store.tmp_dir();
        if tmp.is_dir() {
            for e in std::fs::read_dir(&tmp)?.flatten() {
                let _ = std::fs::remove_file(e.path());
            }
        }
        std::fs::create_dir_all(&tmp)?;
        let total = store
            .ids()?
            .iter()
            .filter_map(|id| store.read_meta(id))
            .fold(0u64, |s, m| s.saturating_add(m.bytes));
        *store.used.lock().unwrap_or_else(PoisonError::into_inner) = total;
        Ok(store)
    }

    pub fn data_dir(&self) -> &Path {
        &self.data
    }

    fn clips_dir(&self) -> PathBuf {
        self.data.join("clips")
    }

    fn tmp_dir(&self) -> PathBuf {
        self.data.join("tmp")
    }

    /// The folder of clip `id` (`None` for anything that isn't an id).
    pub fn clip_dir(&self, id: &str) -> Option<PathBuf> {
        valid_id(id).then(|| self.clips_dir().join(id))
    }

    pub fn video_path(&self, id: &str) -> Option<PathBuf> {
        self.clip_dir(id).map(|d| d.join("video.mp4"))
    }

    pub fn poster_path(&self, id: &str, ext: &str) -> Option<PathBuf> {
        matches!(ext, "jpg" | "png" | "webp")
            .then(|| self.clip_dir(id).map(|d| d.join(format!("poster.{ext}"))))
            .flatten()
    }

    /// A new file name in `tmp/`.
    pub fn tmp_path(&self) -> Result<PathBuf, StoreError> {
        Ok(self.tmp_dir().join(format!("{}.part", random_suffix()?)))
    }

    /// Every stored clip id.
    pub fn ids(&self) -> Result<Vec<String>, StoreError> {
        let mut ids = Vec::new();
        for e in std::fs::read_dir(self.clips_dir())?.flatten() {
            if let Some(name) = e.file_name().to_str()
                && valid_id(name)
            {
                ids.push(name.to_string());
            }
        }
        Ok(ids)
    }

    pub fn read_meta(&self, id: &str) -> Option<Meta> {
        let path = self.clip_dir(id)?.join("meta.json");
        let bytes = read_capped(&path, MAX_META_BYTES).ok()??;
        let meta: Meta = serde_json::from_slice(&bytes).ok()?;
        (meta.id == id).then_some(meta)
    }

    pub fn write_meta(&self, meta: &Meta) -> Result<(), StoreError> {
        let dir = self
            .clip_dir(&meta.id)
            .ok_or_else(|| StoreError::Bad("bad id".into()))?;
        let bytes = serde_json::to_vec_pretty(meta).map_err(|e| StoreError::Bad(e.to_string()))?;
        write_atomic(&dir.join("meta.json"), &bytes)
    }

    /// Move a received upload into place as clip `meta.id` and record it.
    pub fn commit(&self, tmp: &Path, meta: &Meta) -> Result<(), StoreError> {
        let dir = self
            .clip_dir(&meta.id)
            .ok_or_else(|| StoreError::Bad("bad id".into()))?;
        std::fs::create_dir(&dir)?;
        let done = std::fs::rename(tmp, dir.join("video.mp4"))
            .map_err(StoreError::from)
            .and_then(|()| self.write_meta(meta));
        if done.is_err() {
            let _ = std::fs::remove_dir_all(&dir);
        }
        done
    }

    /// Space all clips take now.
    pub fn used(&self) -> u64 {
        *self.used.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Hold `bytes` of the quota for an upload; `false` when it doesn't fit.
    pub fn reserve(&self, bytes: u64, quota: u64) -> bool {
        let mut used = self.used.lock().unwrap_or_else(PoisonError::into_inner);
        match used.checked_add(bytes) {
            Some(n) if n <= quota => {
                *used = n;
                true
            }
            _ => false,
        }
    }

    /// Give back `bytes` (an upload that ended smaller or failed, a deleted clip).
    pub fn release(&self, bytes: u64) {
        let mut used = self.used.lock().unwrap_or_else(PoisonError::into_inner);
        *used = used.saturating_sub(bytes);
    }

    /// Remove clip `id`; whether it was there.
    pub fn delete(&self, id: &str) -> bool {
        let Some(dir) = self.clip_dir(id) else {
            return false;
        };
        let bytes = self.read_meta(id).map(|m| m.bytes);
        if !dir.is_dir() || std::fs::remove_dir_all(&dir).is_err() {
            return false;
        }
        if let Some(b) = bytes {
            self.release(b);
        }
        true
    }

    /// Remove the clips whose time is up; how many.
    pub fn sweep(&self, now_unix: u64) -> usize {
        let ids = self.ids().unwrap_or_default();
        ids.iter()
            .filter(|id| self.read_meta(id).is_some_and(|m| m.expired(now_unix)))
            .filter(|id| self.delete(id))
            .count()
    }

    fn tokens_path(&self) -> PathBuf {
        self.data.join("tokens.json")
    }

    pub fn tokens(&self) -> Result<TokenFile, StoreError> {
        match read_capped(&self.tokens_path(), MAX_TOKENS_BYTES)? {
            None => Ok(TokenFile::default()),
            Some(b) => {
                serde_json::from_slice(&b).map_err(|e| StoreError::Bad(format!("tokens.json: {e}")))
            }
        }
    }

    fn save_tokens(&self, t: &TokenFile) -> Result<(), StoreError> {
        let bytes = serde_json::to_vec_pretty(t).map_err(|e| StoreError::Bad(e.to_string()))?;
        std::fs::create_dir_all(&self.data)?;
        write_atomic(&self.tokens_path(), &bytes)
    }

    /// A new upload token called `name`; the token itself is returned once and only its
    /// hash is kept.
    pub fn add_token(&self, name: &str) -> Result<String, StoreError> {
        if !valid_token_name(name) {
            return Err(StoreError::Bad(
                "a token name is 1 to 40 letters, digits, spaces, dots, _ or -".into(),
            ));
        }
        let mut t = self.tokens()?;
        if t.tokens.iter().any(|e| e.name == name) {
            return Err(StoreError::Bad(format!(
                "there is already a token called {name}"
            )));
        }
        if t.tokens.len() >= MAX_TOKENS {
            return Err(StoreError::Bad(format!("at most {MAX_TOKENS} tokens")));
        }
        let token = klip_share::new_upload_token()
            .map_err(|e| StoreError::Bad(format!("no randomness: {e}")))?;
        t.tokens.push(TokenEntry {
            name: name.into(),
            hash: hash_token(&token),
            created_unix: now_unix(),
        });
        self.save_tokens(&t)?;
        Ok(token)
    }

    /// Remove token `name`; whether it existed.
    pub fn remove_token(&self, name: &str) -> Result<bool, StoreError> {
        let mut t = self.tokens()?;
        let before = t.tokens.len();
        t.tokens.retain(|e| e.name != name);
        if t.tokens.len() == before {
            return Ok(false);
        }
        self.save_tokens(&t)?;
        Ok(true)
    }

    /// The name of the upload token `token`, when it is one.
    pub fn check_token(&self, token: &str) -> Option<String> {
        if !token.starts_with(klip_share::UPLOAD_TOKEN_PREFIX) || token.len() > 200 {
            return None;
        }
        let hash = hash_token(token);
        let file = self.tokens().ok()?;
        let mut found = None;
        // Look at every entry, so the time taken doesn't say which one matched.
        for e in &file.tokens {
            if same_secret(&e.hash, &hash) {
                found = Some(e.name.clone());
            }
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(id: &str, bytes: u64, expires_unix: u64) -> Meta {
        Meta {
            id: id.into(),
            title: "t".into(),
            created_unix: 1,
            expires_unix,
            bytes,
            width: 1,
            height: 1,
            duration_ms: 1,
            codec: "H.264".into(),
            poster: None,
            manage_hash: hash_token("ssm_x"),
            uploader: "pc".into(),
        }
    }

    #[test]
    fn clips_are_committed_counted_swept_and_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let a = "0123456789abcdef0123456789abcdef";
        let b = "fedcba9876543210fedcba9876543210";
        for (id, bytes, exp) in [(a, 10, 0), (b, 5, 100)] {
            let tmp = s.tmp_path().unwrap();
            std::fs::write(&tmp, vec![0u8; bytes as usize]).unwrap();
            assert!(s.reserve(bytes, 1000));
            s.commit(&tmp, &meta(id, bytes, exp)).unwrap();
        }
        assert_eq!(s.used(), 15);
        // A fresh start counts the same and empties tmp/.
        std::fs::write(s.tmp_path().unwrap(), b"half").unwrap();
        let s = Store::open(dir.path()).unwrap();
        assert_eq!(s.used(), 15);
        assert_eq!(
            std::fs::read_dir(dir.path().join("tmp")).unwrap().count(),
            0
        );
        assert_eq!(s.read_meta(a).unwrap().bytes, 10);
        assert!(s.read_meta("../tokens").is_none());
        assert!(s.clip_dir("..").is_none());
        assert!(s.poster_path(a, "svg").is_none());
        // b expires at 100.
        assert_eq!(s.sweep(99), 0);
        assert_eq!(s.sweep(100), 1);
        assert_eq!(s.used(), 10);
        assert!(s.delete(a));
        assert!(!s.delete(a));
        assert_eq!(s.used(), 0);
        assert!(!s.reserve(1001, 1000));
        assert!(s.reserve(1000, 1000));
    }

    #[test]
    fn tokens_are_kept_hashed() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let t = s.add_token("phone").unwrap();
        assert!(s.add_token("phone").is_err());
        for bad in ["", " lead", "a/b", &"x".repeat(41)] {
            assert!(s.add_token(bad).is_err(), "{bad}");
        }
        let file = std::fs::read_to_string(dir.path().join("tokens.json")).unwrap();
        assert!(!file.contains(&t), "the token itself is never stored");
        assert_eq!(s.check_token(&t).as_deref(), Some("phone"));
        assert_eq!(s.check_token("sst_nope"), None);
        assert_eq!(s.check_token(&t.replace("sst_", "ssm_")), None);
        assert!(s.remove_token("phone").unwrap());
        assert!(!s.remove_token("phone").unwrap());
        assert_eq!(s.check_token(&t), None);
        std::fs::write(dir.path().join("tokens.json"), b"{ not json").unwrap();
        assert!(s.tokens().is_err());
        assert_eq!(s.check_token(&t), None);
    }
}
