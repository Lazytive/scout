# klip-media

Pure data types shared by the buffer, the MP4 writer and the capture pipeline:
`Codec`, `TrackInfo` (codec configuration exactly as MP4 stores it), `Packet` (one encoded
access unit with ns timestamps), and `parse_duration` for CLI/config values.

`FrameGrid` snaps capture timestamps to a constant-frame-rate grid starting at the first
frame: every frame is kept, a frame on an occupied slot takes the next one, a missing
frame leaves a gap, and a source faster than the grid restarts it instead of drifting
ahead of real time. Every 30 frames it re-centres on the average arrival phase, so the
first frame's phase doesn't stick. Outputs are strictly increasing for any input.

`DriftAligner` turns audio capture packets (exact capture time + frame count) into a
continuous sample stream on the same timeline: each packet continues the previous one,
single frames are repeated or dropped when the smoothed difference passes half a frame
(clock drift), and gaps over 20 ms restart the timeline.

Limits that protect against hostile media: `MAX_PACKET_BYTES` (64 MiB) and
`MAX_CODEC_DATA_BYTES` (64 KiB). `TrackInfo::validate` rejects inconsistent descriptions.

Layer: pure (no GStreamer, no OS APIs).
