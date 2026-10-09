//! What the library grid shows: the clips that match a search and filters, sorted, with
//! the counts the filter panel needs (per game, per tag, starred).

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::index::{ClipInfo, Index, ms_since_epoch};
use crate::thumbs::ThumbVersions;
use crate::{ClipFile, Meta};

/// Most clips one listing returns.
pub const MAX_LIST: usize = 20_000;
/// Longest search text looked at.
const MAX_SEARCH_CHARS: usize = 200;
/// Creation times before this (2000-01-01) are not believed.
const PLAUSIBLE_FROM_UNIX: u64 = 946_684_800;

/// Sort order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    #[default]
    Newest,
    Oldest,
    Longest,
    Shortest,
    Largest,
    Name,
}

/// A search over the library (`library.list` arguments). Unknown fields are refused.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Query {
    /// Words that must all appear in the clip's name, game or tags (any case).
    pub search: String,
    /// Only clips of this game folder; `""`: clips not in a game folder.
    pub game: Option<String>,
    pub starred: bool,
    /// Only clips with this tag (any case).
    pub tag: Option<String>,
    /// Only clips from this time on (ms since the epoch).
    pub since_ms: Option<u64>,
    pub sort: Sort,
    /// Return at most this many (the counts still cover every match).
    pub limit: Option<usize>,
}

/// One clip in a listing.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ListedClip {
    pub key: String,
    pub path: String,
    /// File name without `.mp4`.
    pub name: String,
    /// Game folder; `None` for clips directly in the clip folder.
    pub game: Option<String>,
    pub bytes: u64,
    /// When the clip was made: the time written in the file if believable, else the
    /// file's modification time (ms since the epoch).
    pub date_ms: u64,
    pub starred: bool,
    pub tags: Vec<String>,
    pub info: Option<ClipInfo>,
    /// Why `info` is missing.
    pub error: Option<String>,
    pub thumbs: ThumbVersions,
}

/// Count of clips with one facet value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Facet {
    /// Game folder (`""`: none) or tag.
    pub name: String,
    pub count: usize,
}

/// What [`query`] returns.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Listing {
    pub clips: Vec<ListedClip>,
    /// Clips matching the query (before `limit`).
    pub matched: usize,
    /// Every clip in the folder.
    pub total: usize,
    pub total_bytes: u64,
    pub starred: usize,
    /// Over every clip, not just the matches, so the filter panel doesn't jump.
    pub games: Vec<Facet>,
    pub tags: Vec<Facet>,
}

/// The game folder of a key (`Game/clip.mp4` → `Game`).
pub fn game_of(key: &str) -> Option<&str> {
    let (dir, _) = key.rsplit_once('/')?;
    Some(dir.split('/').next().unwrap_or(dir))
}

fn name_of(key: &str) -> String {
    let file = key.rsplit('/').next().unwrap_or(key);
    Path::new(file)
        .file_stem()
        .map_or_else(|| file.to_string(), |s| s.to_string_lossy().into_owned())
}

fn date_ms(f: &ClipFile, info: Option<&ClipInfo>, now_ms: u64) -> u64 {
    let modified = ms_since_epoch(f.modified);
    let created = info.map_or(0, |i| i.created_unix);
    let latest = now_ms.saturating_add(86_400_000);
    if created >= PLAUSIBLE_FROM_UNIX {
        let ms = created.saturating_mul(1000);
        if ms <= latest {
            return ms;
        }
    }
    modified
}

fn words(search: &str) -> Vec<String> {
    search
        .chars()
        .take(MAX_SEARCH_CHARS)
        .collect::<String>()
        .to_lowercase()
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// List the clips matching `q`. `now_ms`: the current time (for believable dates).
pub fn query(
    files: &[ClipFile],
    meta: &Meta,
    index: &Index,
    thumbs: &HashMap<String, ThumbVersions>,
    q: &Query,
    now_ms: u64,
) -> Listing {
    let words = words(&q.search);
    let tag = q.tag.as_ref().map(|t| t.to_lowercase());
    let mut listing = Listing::default();
    let mut games: BTreeMap<String, usize> = BTreeMap::new();
    let mut tags: BTreeMap<String, (String, usize)> = BTreeMap::new();
    let mut out: Vec<ListedClip> = Vec::new();
    for f in files {
        let m = meta.clips.get(&f.key);
        let starred = m.is_some_and(|m| m.starred);
        let clip_tags = m.map(|m| m.tags.clone()).unwrap_or_default();
        let game = game_of(&f.key).map(str::to_string);
        listing.total = listing.total.saturating_add(1);
        listing.total_bytes = listing.total_bytes.saturating_add(f.bytes);
        if starred {
            listing.starred = listing.starred.saturating_add(1);
        }
        let g = games.entry(game.clone().unwrap_or_default()).or_default();
        *g = g.saturating_add(1);
        for t in &clip_tags {
            let e = tags
                .entry(t.to_lowercase())
                .or_insert_with(|| (t.clone(), 0));
            e.1 = e.1.saturating_add(1);
        }

        if q.starred && !starred {
            continue;
        }
        if let Some(want) = &q.game
            && game.as_deref().unwrap_or_default() != want
        {
            continue;
        }
        if let Some(want) = &tag
            && !clip_tags.iter().any(|t| t.to_lowercase() == *want)
        {
            continue;
        }
        let entry = index.clips.get(&f.key);
        let info = entry.and_then(|e| e.info.as_ref());
        let date = date_ms(f, info, now_ms);
        if q.since_ms.is_some_and(|s| date < s) {
            continue;
        }
        let name = name_of(&f.key);
        if !words.is_empty() {
            let mut hay = name.to_lowercase();
            if let Some(g) = &game {
                hay.push('\n');
                hay.push_str(&g.to_lowercase());
            }
            for t in &clip_tags {
                hay.push('\n');
                hay.push_str(&t.to_lowercase());
            }
            if !words.iter().all(|w| hay.contains(w.as_str())) {
                continue;
            }
        }
        out.push(ListedClip {
            key: f.key.clone(),
            path: f.path.to_string_lossy().into_owned(),
            name,
            game,
            bytes: f.bytes,
            date_ms: date,
            starred,
            tags: clip_tags,
            info: info.cloned(),
            error: entry.and_then(|e| e.error.clone()),
            thumbs: thumbs.get(&f.key).copied().unwrap_or_default(),
        });
    }
    let duration = |c: &ListedClip| c.info.as_ref().and_then(|i| i.duration_ms).unwrap_or(0);
    out.sort_by(|a, b| {
        let primary = match q.sort {
            Sort::Newest => b.date_ms.cmp(&a.date_ms),
            Sort::Oldest => a.date_ms.cmp(&b.date_ms),
            Sort::Longest => duration(b).cmp(&duration(a)),
            Sort::Shortest => duration(a).cmp(&duration(b)),
            Sort::Largest => b.bytes.cmp(&a.bytes),
            Sort::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        };
        primary
            .then_with(|| b.date_ms.cmp(&a.date_ms))
            .then_with(|| a.key.cmp(&b.key))
    });
    listing.matched = out.len();
    out.truncate(q.limit.unwrap_or(MAX_LIST).min(MAX_LIST));
    listing.clips = out;
    listing.games = games
        .into_iter()
        .map(|(name, count)| Facet { name, count })
        .collect();
    let mut tags: Vec<Facet> = tags
        .into_values()
        .map(|(name, count)| Facet { name, count })
        .collect();
    tags.sort_by(|a, b| match b.count.cmp(&a.count) {
        Ordering::Equal => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        o => o,
    });
    listing.tags = tags;
    listing
}
