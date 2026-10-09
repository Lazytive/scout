# klip-library

The clip folder (ADR 0014). Clips stay plain MP4 files in a normal folder; Klip's own
metadata lives in `<folder>/.klip/library.json` (stars now, tags and titles with the
library in M7). Losing that file loses stars, never clips. Pure: file I/O only.

- `Library::load_meta` / `save_meta` (atomic). Hostile files: over 16 MiB or not valid
  JSON → moved aside to `library.json.bad`, start empty; keys must be clean relative
  paths (`/` separators, no `..`, no drive letters, no empty parts); at most 200 000
  entries. `set_starred(path, bool)`; `key(path)` (relative to the folder).
- `scan()`: every finished `.mp4` in the folder and one level of game folders. Skipped:
  recordings still being written (they have a `.recording.json` sidecar), temp files,
  dot folders (`.klip`), deeper folders. A missing folder scans as empty.
- `over_cap(files, meta, cap_bytes, protect)`: the clips to remove so the folder fits
  the cap: oldest (modification time) unstarred first, never a starred or protected one
  (the clip just saved); if starred + protected clips alone exceed the cap, all others
  go. `cap_bytes = 0`: no cap. `usage()`: counts and bytes, starred included.
- `forget(meta, keys)`: drop entries of clips Klip removed. Entries of clips deleted
  elsewhere are kept (the file may be on a drive that is unplugged right now).
- Units: the storage settings are in GiB (`GIB`), like Explorer shows sizes.

Moving files to the Recycle Bin / Trash is `klip_platform::move_to_trash`; the engine
runs the policy (`klip-engine`, `engine/storage.rs`).

Tests: scan rules, stars round trip, bad and hostile metadata, the cap policy (order,
stars, protection, no cap), forgetting.

Layer: pure.
