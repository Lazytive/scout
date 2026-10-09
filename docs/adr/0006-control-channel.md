# 0006: loopback JSON-lines control channel with a token file

Status: accepted (M0)

## Context
"Everything is a command": the UI, the CLI (`klip clip` bound to a desktop shortcut) and
agents drive the recorder through the same command ids. It must work on Windows and Linux
and must not let other users, or web pages, trigger it.

## Decision
- The recorder process listens on `127.0.0.1:<ephemeral port>`. It writes
  `{"port", "token", "pid"}` atomically to a discovery file in the per-user runtime dir
  (`%LOCALAPPDATA%\Klip\control.json`, `$XDG_RUNTIME_DIR/klip/control.json`).
- The token is 32 random bytes (hex). The first line of every connection must be
  `{"auth":"<token>"}`, compared in constant time. Browsers can't read the file, and an
  HTTP request line fails auth and closes the connection.
- Protocol: one JSON object per line. Request `{"id":1,"cmd":"clip.save","args":{…}}`,
  response `{"id":1,"ok":true,"result":{…}}` or
  `{"id":1,"ok":false,"error":{"code":"…","message":"…"}}`. Lines are capped (64 KiB),
  concurrent connections are capped, unknown fields are rejected.
- Dev-only commands (test hooks, screenshots) need the `--dev` flag on the recorder.

## Consequences
No extra dependency (std TCP + serde_json). Same code path on both OSes. Later the Tauri app
hosts the same command registry in-process and exposes this channel for the CLI.
