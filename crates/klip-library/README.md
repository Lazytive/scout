# klip-library

The clip folder (ADR 0014, ADR 0015). Clips stay plain MP4 files in a normal folder; Klip's
own data lives in `<folder>/.klip/`: `library.json` (stars and tags: the user's), and two
caches rebuilt from the clips (`index.json`, `thumbs/`). Losing `.klip/` loses stars and
tags, never clips. Pure: file I/O only.

- `Library::load_meta` / `save_meta` (atomic). Hostile files: over 16 MiB or not valid
  JSON → moved aside to `library.json.bad`, start empty; keys must be clean relative
  paths (`/` separators, no `..`, no drive letters, no empty parts); at most 200 000
  entries; tags cleaned and capped. `set_starred(path, bool)`, `set_tags(path, tags)`;
  `key(path)` (relative to the folder).
- Names (`names.rs`): `check_clip_name` (Windows-safe: no `<>:"/\|?*` or control
  characters, no leading/trailing dot, no reserved device names, ≤ 120 characters; a
  bad name is refused with a reason, never changed silently), `normalize_tags` (≤ 32
  tags of ≤ 40 characters, whitespace collapsed, duplicates ignoring case dropped).
- `rename(path, name)`: renames the file in its folder (`.mp4` kept), refuses to replace
  another file (a case-only rename on Windows is allowed) or a recording still being
  written; stars, tags and thumbnails move to the new key.
- `scan()`: every finished `.mp4` in the folder and one level of game folders. Skipped:
  recordings still being written (they have a `.recording.json` sidecar), temp files,
  dot folders (`.klip`), deeper folders. A missing folder scans as empty.
- Info index (`index.rs`): `load_index` / `save_index` / `refresh_index(files, index)`:
  per clip its size and modification time and `ClipInfo` read by `klip_mp4::probe`
  (duration, creation time, codec, width, height, fps, audio tracks, bookmark times) or
  the error; changed files are probed again, gone ones dropped; `thumbs_failed` keeps a
  clip whose thumbnails failed from being retried until it changes. Unreadable → empty
  (it's a cache).
- Thumbnail cache (`thumbs.rs`): `thumb_path(key, Poster|Strip)` (`<key>.jpg` /
  `<key>.strip.jpg`, `%` → `%25`, `/` → `%2F`), `thumb_versions()` (one directory
  listing), `move_thumbs`, `remove_thumbs`, `prune_thumbs(keys)` (orphans and temp files
  older than an hour). Sizes: posters 320 px, strips 16 frames of 240 px.
- Per-clip caches (`caches.rs`, M8.4): `project_path(key)` (`.klip/projects/<key>.json`,
  the editor's project: a user's edits, kept while the clip exists), `proxy_path(key)`
  (`.klip/proxies/<key>.mp4`), `render_dir(key)` (`.klip/render/<key>/`); keys escaped
  like thumbnails; `move_caches` (called by `rename`), `remove_caches`, `prune_caches
  (keys)` (only with a complete key set). `unique_sibling(clip, suffix)`: a free
  `<name> - <suffix>.mp4` next to the clip (` (2)`, ` (3)`…; also skips a `.part` being
  written; the name shortened to stay a valid clip name).
- `query(files, meta, index, thumbs, q, now)` (`query.rs`): the library grid's listing:
  search words over name + game folder + tags, filters (game folder, `""` = none;
  starred; tag; since), sorts (newest, oldest, longest, shortest, largest, name), counts
  per game and tag over all clips, `limit`. A clip's date is the creation time in the
  file when believable (after 2000, not in the future), else its modification time.
  `Query` refuses unknown fields (it is the `library.list` argument).
- `over_cap(files, meta, cap_bytes, protect)`: the clips to remove so the folder fits
  the cap: oldest (modification time) unstarred first, never a starred or protected one
  (the clip just saved); if starred + protected clips alone exceed the cap, all others
  go. `cap_bytes = 0`: no cap. `usage()`: counts and bytes, starred included.
- `forget(meta, keys)`: drop entries of clips Klip removed. Entries of clips deleted
  elsewhere are kept (the file may be on a drive that is unplugged right now).
- Units: the storage settings are in GiB (`GIB`), like Explorer shows sizes.

Moving files to the Recycle Bin / Trash is `klip_platform::move_to_trash`; the engine
runs the policy (`klip-engine`, `engine/storage.rs`) and the library commands
(`engine/library.rs`).

Tests: scan rules, stars round trip, bad and hostile metadata, the cap policy, forgetting;
(M7) clip names, tags (and a proptest that neither panics on any input), rename (meta
and thumbs follow, no overwrite, recordings, case-only on Windows), the index (probes
only new/changed clips, hostile index files), queries (every filter and sort, facets,
strict arguments), thumbnail versions and pruning.

Layer: pure (depends on `klip-mp4`).
