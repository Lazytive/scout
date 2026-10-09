# klip-media

Pure data types shared by the buffer, the MP4 writer and the capture pipeline:
`Codec`, `TrackInfo` (codec configuration exactly as MP4 stores it), `Packet` (one encoded
access unit with ns timestamps), and `parse_duration` for CLI/config values.

Limits that protect against hostile media: `MAX_PACKET_BYTES` (64 MiB) and
`MAX_CODEC_DATA_BYTES` (64 KiB). `TrackInfo::validate` rejects inconsistent descriptions.

Layer: pure (no GStreamer, no OS APIs).
