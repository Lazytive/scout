# 0015: Library: plain files, rebuildable caches, thumbnails in a child process

Status: accepted (M7, 2026-10-09).

## Context
M7 adds the Library: a grid of the clips in the clip folder with thumbnails and
hover-scrub, search, filters (game, date, starred, tag), star, rename, tags and delete
(KLIP_PROMPT §3). The brief says clips stay plain MP4s in a normal folder (no lock-in),
and any index must be rebuildable from the folder. The folder is the user's: it can hold
files from other programs, half-copied files and broken ones, and it is read while a
game is being recorded, so nothing the library does may risk the replay buffer or cost
the game frames.

## Decision
- **Files are the truth.** The library is `klip_library::Library::scan()` (the clip
  folder and one level of game folders, as in M6). Rename renames the file (same folder,
  `.mp4` kept, never replacing another file; Windows-safe names checked, not silently
  changed). Delete moves to the Recycle Bin / Trash; `permanent: true` (Shift+Delete in
  the UI, as in Explorer) deletes. A separate recordings folder is not part of the
  library yet.
- **User metadata** stays in `.klip/library.json` (schema 1, additive): `starred` and now
  `tags` (cleaned: whitespace collapsed, no control characters, ≤ 40 characters, ≤ 32
  per clip, duplicates ignoring case dropped). Keys follow renames. Losing the file loses
  stars and tags, never clips.
- **Info cache** `.klip/index.json`: per clip key, the file's size and modification time
  and what `klip_mp4::probe` read from its `moov` (duration, creation time, video codec,
  size and average fps, audio track count, chapter times = bookmarks), or the error.
  Entries that don't match the file are probed again; entries of clips that are gone are
  dropped. It is a cache: unreadable or of another version → rebuilt, no `.bad` copy.
  Probing reads only boxes (no samples): pure Rust, hostile-input safe (caps on the
  `moov` size, tracks, chapters; a track or `udta` that doesn't parse is skipped).
- **Thumbnails** in `.klip/thumbs/`: a poster (`<name>.jpg`, 320 px, 0.5 s before the end:
  the moment of the hotkey, as the overlay shows it) and a hover-scrub strip
  (`<name>.strip.jpg`: 16 frames of 240 px, the key frame nearest the middle of each
  sixteenth, stacked top to bottom in one JPEG, quality 75). `<name>` is the key with `%`
  → `%25` and `/` → `%2F` (collision-free; the M6 `~` naming could collide). Orphans are
  pruned when the library is first listed.
- **Hover-scrub = sprite strip, not `<video>` seeking.** A strip is one small image per
  clip: scrubbing is instant (a CSS background position), works for every codec GStreamer
  can decode (WebView2 can't play HEVC without a paid extension) and keeps no decoder
  open per card. The editor (M8) will need a `<video>` path anyway; that's its own
  decision.
- **Decoding happens in a child process.** Posters and strips for the library are made
  by a media worker: the app (or `klip-cli`) re-run with `--klip-media-worker`
  (`klip_engine::media_worker`), JSON lines over stdin/stdout, outputs written
  atomically, untrusted replies size-capped and parsed strictly, 60 s per job then the
  worker is killed and replaced, stopped after 20 s idle, below-normal priority on
  Windows. A demuxer or decoder that crashes or hangs on a bad file therefore costs one
  thumbnail, never the app or a game's replay buffer. (The overlay's poster of a clip
  Klip just wrote is still made in-process: it is our own file and the pop-up needs it
  fast.) Tests run jobs in-process (`EngineOptions::media_worker = None`).
- **Scheduling.** A `klip-thumbs` thread takes the clips the UI asks for (cards on
  screen, `library.thumbs`) first, then every other clip missing thumbnails, newest
  first, but that background work waits while a game is being recorded (the game ending
  wakes it). A clip whose thumbnails failed is not tried again until its file changes.
- **Serving** to the WebView: a `klipthumb` URI scheme registered by the app
  (`http://klipthumb.localhost/<poster|strip>/<key>?v=<mtime>` on Windows). It answers
  only for cached thumbnails of clips in the library (the engine's `library.thumb`
  validates the key), reads at most 8 MiB, and lets the WebView cache by URL (the
  version is in it). The CSP allows it for `img-src` only. No asset-protocol access to
  arbitrary files.
- **Search, filters and sorting** are `klip_library::query` (pure, unit-tested), behind
  `library.list` (args = `Query`, unknown fields refused): words must all appear in the
  name, game folder or tags (any case); filters game (`""` = not in a game folder),
  starred, tag, since; sorts newest/oldest (by the file's own creation time when
  believable, else its modification time), longest/shortest, largest, name; counts per
  game and tag over the whole folder. The UI groups date sorts by day.
- **Commands** (all also on the control channel and `klip-cli call`): `library.list`,
  `library.thumbs`, `library.thumb`, `clip.rename`, `clip.tags`, `clip.delete`,
  `clip.open` (default app), `clip.reveal` (file manager, selected), `clip.star`.
  Events: `library_changed` (rename, tags, star, delete), `thumbs_ready` (key, versions,
  failed).
- **Store detection switch.** `games.detect_stores` (default on). Off: only the user's
  list counts. Tests turn it off so they can never follow a game the human is playing
  (an M7 e2e run followed a running store game before this).

## Consequences
- The library works on any folder of MP4s and survives losing `.klip/` (stars and tags
  go, everything else is rebuilt).
- Strips cost disk: ~300 KB for the test game's busy pattern, so roughly 0.5 % of a
  50 GB clip folder; measured sizes for real games belong in ROADMAP.
- First listing of a big folder probes every clip once (reading `moov` boxes only).
- Linux: `clip.open` uses `xdg-open`, `clip.reveal` the FileManager1 D-Bus interface via
  `gdbus` (falls back to opening the folder); not yet run on Linux.
