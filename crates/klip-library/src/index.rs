//! The info cache, `<folder>/.klip/index.json`: what [`klip_mp4::probe`] read from each
//! clip (duration, size, fps, codec, bookmarks), keyed by the clip's path and checked
//! against its size and modification time. Rebuilt from the clips whenever it is
//! missing, unreadable or out of date, so it never needs a repair.

use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::{ClipFile, Library, LibraryError, MAX_CLIPS, META_DIR, clean_rel, write_atomic};

/// Index file inside [`META_DIR`].
pub const INDEX_FILE: &str = "index.json";
/// Index files bigger than this are not read.
pub const MAX_INDEX_BYTES: u64 = 64 << 20;
/// Most bookmarks kept per clip.
const MAX_BOOKMARKS: usize = 255;
/// Longest codec name kept.
const MAX_CODEC_CHARS: usize = 16;
/// Longest error kept.
const MAX_ERROR_CHARS: usize = 200;

/// What a clip holds, as far as the library shows it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipInfo {
    pub duration_ms: Option<u64>,
    /// Creation time written in the file (seconds since the Unix epoch, 0: none).
    pub created_unix: u64,
    pub codec: Option<String>,
    pub width: u32,
    pub height: u32,
    pub fps: Option<f64>,
    pub audio_tracks: u32,
    /// Bookmark times, ms from the start (every chapter of the file).
    pub bookmarks_ms: Vec<u64>,
    /// Chapters with a title of their own (an auto-clip event, a chapter another tool
    /// wrote), not Klip's `Bookmark <n>`: the editor shows them as event markers.
    pub events: Vec<ClipEvent>,
}

/// A titled chapter of a clip.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipEvent {
    pub at_ms: u64,
    pub title: String,
}

/// Longest event title kept.
const MAX_EVENT_CHARS: usize = 80;

/// Klip's own bookmark titles (`Bookmark`, `Bookmark 3`) and untitled chapters.
fn is_bookmark_title(title: &str) -> bool {
    let t = title.trim();
    t.is_empty()
        || t.strip_prefix("Bookmark")
            .is_some_and(|rest| rest.trim().chars().all(|c| c.is_ascii_digit()))
}

fn clean_title(title: &str) -> String {
    title
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_EVENT_CHARS)
        .collect::<String>()
        .trim()
        .to_string()
}

impl ClipInfo {
    fn from_mp4(i: &klip_mp4::Mp4Info) -> ClipInfo {
        let v = i.video.as_ref();
        ClipInfo {
            duration_ms: i.duration_ms,
            created_unix: i.creation_time_unix,
            codec: v.map(|v| v.codec.chars().take(MAX_CODEC_CHARS).collect()),
            width: v.map_or(0, |v| v.width),
            height: v.map_or(0, |v| v.height),
            fps: v.and_then(|v| v.fps),
            audio_tracks: i.audio_tracks,
            bookmarks_ms: i.chapters_ms.iter().copied().take(MAX_BOOKMARKS).collect(),
            events: i
                .chapters_ms
                .iter()
                .zip(&i.chapter_titles)
                .filter(|(_, t)| !is_bookmark_title(t))
                .take(MAX_BOOKMARKS)
                .map(|(&at_ms, t)| ClipEvent {
                    at_ms,
                    title: clean_title(t),
                })
                .collect(),
        }
    }

    /// Clamp what a hostile index file may hold.
    fn sanitise(&mut self) {
        if let Some(c) = &mut self.codec {
            *c = c.chars().take(MAX_CODEC_CHARS).collect();
        }
        self.bookmarks_ms.truncate(MAX_BOOKMARKS);
        self.events.truncate(MAX_BOOKMARKS);
        for e in &mut self.events {
            e.title = clean_title(&e.title);
        }
        if self
            .fps
            .is_some_and(|f| !f.is_finite() || f <= 0.0 || f > 1000.0)
        {
            self.fps = None;
        }
    }
}

/// One clip in the index.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct IndexEntry {
    /// The file this entry describes: size and modification time (ms since the epoch).
    pub bytes: u64,
    pub modified_ms: u64,
    /// `None` when the file couldn't be read as an MP4 (`error` says why).
    pub info: Option<ClipInfo>,
    pub error: Option<String>,
    /// Making its thumbnails failed; not retried until the file changes.
    pub thumbs_failed: bool,
}

/// `index.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Index {
    pub version: u32,
    pub clips: BTreeMap<String, IndexEntry>,
}

impl Default for Index {
    fn default() -> Self {
        Index {
            version: 1,
            clips: BTreeMap::new(),
        }
    }
}

pub(crate) fn ms_since_epoch(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

impl IndexEntry {
    fn matches(&self, f: &ClipFile) -> bool {
        self.bytes == f.bytes && self.modified_ms == ms_since_epoch(f.modified)
    }
}

/// Read a clip's info (no sample data is read).
pub fn probe_file(path: &Path) -> Result<ClipInfo, String> {
    let f = File::open(path).map_err(|e| e.to_string())?;
    let info =
        klip_mp4::probe(&mut BufReader::with_capacity(64 * 1024, f)).map_err(|e| e.to_string())?;
    Ok(ClipInfo::from_mp4(&info))
}

/// The names of a file's audio tracks in track order (`Mix`, `Game`, `Microphone`, …;
/// empty for an unnamed track), one per audio track.
pub fn audio_track_names(path: &Path) -> Result<Vec<String>, String> {
    let f = File::open(path).map_err(|e| e.to_string())?;
    let info =
        klip_mp4::probe(&mut BufReader::with_capacity(64 * 1024, f)).map_err(|e| e.to_string())?;
    let n = usize::try_from(info.audio_tracks).unwrap_or(0);
    let mut names = info.audio_names;
    names.resize(n, String::new());
    Ok(names)
}

impl Library {
    pub fn index_path(&self) -> std::path::PathBuf {
        self.root.join(META_DIR).join(INDEX_FILE)
    }

    /// The cached index; empty when missing or unreadable (it is rebuilt).
    pub fn load_index(&self) -> Index {
        let Ok(f) = File::open(self.index_path()) else {
            return Index::default();
        };
        let mut bytes = Vec::new();
        let ok = f
            .take(MAX_INDEX_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)
            .is_ok_and(|n| u64::try_from(n).unwrap_or(u64::MAX) <= MAX_INDEX_BYTES);
        let parsed = ok
            .then(|| serde_json::from_slice::<Index>(&bytes).ok())
            .flatten();
        match parsed {
            Some(mut ix) if ix.version == 1 => {
                ix.clips.retain(|k, _| clean_rel(k));
                while ix.clips.len() > MAX_CLIPS {
                    ix.clips.pop_last();
                }
                for e in ix.clips.values_mut() {
                    if let Some(i) = &mut e.info {
                        i.sanitise();
                    }
                    if let Some(err) = &mut e.error {
                        *err = err.chars().take(MAX_ERROR_CHARS).collect();
                    }
                }
                ix
            }
            _ => Index::default(),
        }
    }

    pub fn save_index(&self, index: &Index) -> Result<(), LibraryError> {
        let text = serde_json::to_vec(index).map_err(std::io::Error::other)?;
        write_atomic(&self.index_path(), &text)?;
        Ok(())
    }

    /// Bring `index` up to date with `files`: probe new and changed clips, drop entries of
    /// clips that are gone (only when the scan is complete). Returns whether anything
    /// changed.
    pub fn refresh_index(&self, files: &[ClipFile], index: &mut Index) -> bool {
        let mut changed = false;
        for f in files {
            if index.clips.get(&f.key).is_some_and(|e| e.matches(f)) {
                continue;
            }
            let (info, error) = match probe_file(&f.path) {
                Ok(i) => (Some(i), None),
                Err(e) => (None, Some(e.chars().take(MAX_ERROR_CHARS).collect())),
            };
            index.clips.insert(
                f.key.clone(),
                IndexEntry {
                    bytes: f.bytes,
                    modified_ms: ms_since_epoch(f.modified),
                    info,
                    error,
                    thumbs_failed: false,
                },
            );
            changed = true;
        }
        if files.len() < MAX_CLIPS {
            let keys: HashSet<&str> = files.iter().map(|f| f.key.as_str()).collect();
            let before = index.clips.len();
            index.clips.retain(|k, _| keys.contains(k.as_str()));
            changed |= index.clips.len() != before;
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titled_chapters_are_events() {
        let info = klip_mp4::Mp4Info {
            chapters_ms: vec![100, 200, 300, 400],
            chapter_titles: vec![
                "Bookmark 1".into(),
                "Triple kill".into(),
                String::new(),
                format!("  {}  ", "x".repeat(200)),
            ],
            ..Default::default()
        };
        let c = ClipInfo::from_mp4(&info);
        assert_eq!(c.bookmarks_ms, vec![100, 200, 300, 400]);
        assert_eq!(c.events.len(), 2);
        assert_eq!(
            c.events[0],
            ClipEvent {
                at_ms: 200,
                title: "Triple kill".into()
            }
        );
        assert_eq!((c.events[1].at_ms, c.events[1].title.len()), (400, 78));
        assert!(is_bookmark_title("Bookmark"));
        assert!(is_bookmark_title(" Bookmark 12 "));
        assert!(!is_bookmark_title("Bookmarked kill"));
    }
}
