# klip-control

"Everything is a command" (ADR 0006).

- `commands`: command ids shared by the UI, CLI and control channel (`clip.save`,
  `recorder.status`, `recorder.stop`, `ping`, `session.start`, `session.stop`,
  `session.toggle`, `bookmark.add`).
- `Handler`: implemented by whoever executes commands (the recorder now, the app later).
- `Server`: listens on `127.0.0.1:<ephemeral>`, writes a discovery file
  (`%LOCALAPPDATA%\Klip\control.json` / `$XDG_RUNTIME_DIR/klip/control.json`) with the
  port, a 256-bit random token and the pid. First line of each connection must be
  `{"auth":"<token>"}` (constant-time compare); then JSON-lines requests/responses.
  Limits: 64 KiB lines, 8 connections, 8 malformed requests per connection, 60 s idle.
  Handler panics become `internal` error responses. The discovery file is removed on
  shutdown if it's still ours.
- `Client`: connects via the discovery file, authenticates, `call(cmd, args)`.

Tests cover auth (wrong token, HTTP request), malformed/oversized lines, panicking
handlers, hostile discovery files, concurrent clients.

Layer: pure (std networking only).
