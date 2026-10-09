# klip-buffer

The replay buffer: a ring of **encoded** packets per track (video + audio tracks), bounded
by duration and bytes.

- The first video track is the anchor. Eviction drops whole GOPs from its front, so the
  oldest anchor packet is always a keyframe. The ring keeps at least `max_duration` (up to
  one GOP more), and never more than `max_bytes` in total (hard cap, even if that means
  dropping below `max_duration`).
- Other tracks are trimmed to the anchor's first packet.
- `snapshot(length)` copies (refcounts) the packets from the keyframe at or before
  `end - length` to the end. With 1-second GOPs a 30 s request gives 30–31 s.
  `snapshot_until(Some(t), length)` ends at the last video packet starting before `t`
  (saves cut at the request time while waiting for audio). Snapshot tracks carry their
  ring index (`TrackSnapshot::track`); tracks without data are left out.
- `tail_from_last_keyframe()`: every described track from the newest video keyframe on
  (other tracks from that keyframe's dts), packets possibly empty; starts a full-session
  recording with what is already buffered.
- `track_end`, `anchor_end`, `active_secondary_tracks` let the recorder see whether audio
  has caught up.
- A new codec description (`set_track_info` with different SPS/PPS) drops that track's
  packets, since old and new packets can't share one MP4 sample description.
- Hostile input (oversized packets, timestamps going backwards, delta frames before the
  first keyframe, unknown tracks, extreme timestamps) is refused or saturated, never panics.
  Covered by unit tests and a property test.

Not thread-safe by itself; the recorder wraps it in a mutex and holds the lock only for
push/snapshot (no I/O under the lock).

Layer: pure.
