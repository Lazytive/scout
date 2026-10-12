# klip-share

The share protocol (M11, ADR 0021): what the share server (`apps/klip-share-server`,
binary `scout-share`) and Scout agree on. Pure: no networking, no GStreamer.

- Types: `ServerInfo` (`GET /api/v1/info`), `Uploaded` (`POST /api/v1/clips` → 201),
  `ApiError` (every error), `UploadQuery` (`title`, `expires_days`).
- Paths: `PATH_INFO`, `PATH_CLIPS`, `PATH_PUBLIC` (`/c`).
- Ids: 16 random bytes as lowercase hex (`new_id`, `valid_id`, checked before an id
  names a folder). Tokens: `sst_` (upload) / `ssm_` (manage, one per upload) + 32
  random bytes as hex; stored as `hash_token` (SHA-256), compared with `same_secret`.
- Input checks: `clean_title` (no control characters, ≤ 120 characters),
  `poster_kind` (JPEG / PNG / WebP by their first bytes), `MAX_POSTER_BYTES` (2 MiB).
