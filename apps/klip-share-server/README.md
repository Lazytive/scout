# scout-share: Scout's share server

A small server you run yourself (M11.6, ADR 0021). Scout uploads a clip to it and
copies a link; the link opens a page with the video, and Discord (and most chat apps)
play it right in the chat. Nothing is public by default: links are unlisted (a 128-bit
random id, no index page, `noindex` on everything), and only people with an upload
token can upload.

## Run it

Discord only shows the video in the chat when it can reach the link, so the server
needs an address on the internet with HTTPS. Two easy ways on a home server:

- **Tailscale Funnel**: `tailscale funnel 8080` gives `https://<machine>.<tailnet>.ts.net`
  without opening ports.
- **Caddy** in front (gets the HTTPS certificate for a domain or a DuckDNS name).

### With Docker (M11.7)

On the home server, from a copy of this repository:

1. **Tailscale Funnel** (`compose.yaml`): set `SCOUT_SHARE_PUBLIC_URL` in it to the
   machine's Funnel address (`https://<machine>.<tailnet>.ts.net`), then
   ```bash
   docker compose -f apps/klip-share-server/compose.yaml up -d --build
   sudo tailscale funnel --bg 8080
   ```
   The container only listens on 127.0.0.1:8080; Funnel is the way in.
2. **Your own domain** (`compose.caddy.yaml`, ports 80 and 443 open to the machine):
   ```bash
   SCOUT_SHARE_DOMAIN=clips.example.com docker compose -f apps/klip-share-server/compose.caddy.yaml up -d --build
   ```
3. Make a token (printed once) and put it with the address into Scout (Settings →
   Sharing):
   ```bash
   docker compose -f apps/klip-share-server/compose.yaml exec share scout-share token add my-pc
   ```

The image runs as user 10001 with the data in the `/data` volume (back that up), has a
healthcheck (`scout-share health`) and stops at once on `docker stop`. CI builds it and
drives it with curl on every change (`.github/workflows/share-server.yml`).

### Without Docker

```bash
cargo build --release -p klip-share-server
SCOUT_SHARE_PUBLIC_URL=https://clips.example.com SCOUT_SHARE_DATA_DIR=/srv/scout-share ./target/release/scout-share serve
```

Then make a token for each person or device that uploads, and put it in Scout
(Settings → Sharing, with the server's address):

```bash
scout-share token add my-pc
```

The token is printed once; only its hash is kept (`tokens.json`). `scout-share token
list` and `scout-share token remove NAME` manage them. The token commands read the
same settings as the server (at least `SCOUT_SHARE_DATA_DIR`).

## Settings

A TOML file (`--config FILE` or `SCOUT_SHARE_CONFIG`), and every key as an environment
variable `SCOUT_SHARE_<KEY>` (which wins).

| Key | Default | What |
|---|---|---|
| `public_url` | (needed) | How people reach the server, e.g. `https://clips.example.com`; links and embeds use it |
| `listen` | `0.0.0.0:8080` | Address and port |
| `data_dir` | `data` | Clips, tokens, uploads in progress |
| `max_upload_mb` | 500 | Largest upload (MB) |
| `quota_gb` | 20 | Most space all clips may take (GB) |
| `max_expiry_days` | 0 | Longest a link may last; 0 = no limit (links may last forever) |
| `default_expiry_days` | 0 | When an upload doesn't say; 0 = forever (must be 1…max when max is set) |
| `max_concurrent_uploads` | 4 | More at once get "busy" (429) |
| `site_name` | Scout | Shown on the page and in embeds |
| `upload_idle_secs` | 60 | An upload that sends nothing this long is dropped |

## What's stored

`data_dir/clips/<id>/video.mp4`, `meta.json` (title, size, dimensions, expiry, the
hash of the upload's manage token, the uploading token's name), `poster.jpg|png|webp`;
`data_dir/tokens.json`; `data_dir/tmp/` (uploads in progress, emptied at start). Back up
`data_dir` to keep clips and tokens. Expired clips are removed every hour.

## API

See `crates/klip-share` (types). Tokens go in `Authorization: Bearer …`.

| Request | Token | Answer |
|---|---|---|
| `GET /api/v1/info` | upload | limits, space used, the token's name |
| `POST /api/v1/clips?title=…&expires_days=…`, body = the MP4 | upload | 201 `{id, url, video_url, manage_token, expires_unix, bytes}` |
| `PUT /api/v1/clips/{id}/poster`, body = JPEG/PNG/WebP ≤ 2 MiB | manage | 204 |
| `DELETE /api/v1/clips/{id}` | manage | 204 |
| `GET /c/{id}` · `/c/{id}.mp4` (Range) · `/c/{id}.jpg` | none | page · video · poster |

Errors are JSON `{error, message}`: `unauthorized` 401, `too_big` 413, `not_mp4` 415,
`not_picture` 415, `quota` 507, `busy` 429, `timeout` 408, `bad_expiry` / `bad_query` /
`empty` / `broken` 400, `not_found` 404.

## Safety

- Uploads stream to disk under the size cap and the idle timeout, count against the
  quota before they're kept, and must be an MP4 with a video track (the boxes are read
  with `klip_mp4::probe`; the server runs no decoder). Posters are checked by their
  first bytes. Ids are checked before they name a folder. Titles are cleaned and
  escaped. Every response says `nosniff`, `no-referrer`, `noindex` and a strict
  Content-Security-Policy (the page has no script).
- The server speaks plain HTTP; put HTTPS in front (Caddy, Tailscale Funnel), which
  also limits slow or huge request headers.
- Logs say ids, sizes and token names; never titles, tokens or file contents.

## Tests

`cargo test -p klip-share-server`: settings, store (commit, quota, sweep, tokens kept
hashed), the page (escaping, Open Graph tags), and `tests/server.rs` over real HTTP:
the life of a clip (upload, page, whole and ranged video, poster, delete) and hostile
requests (no/wrong token, not an MP4, too big declared and chunked, a stalled upload,
too many at once, no room, bad ids, an expired link).
