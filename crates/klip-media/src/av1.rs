//! AV1 codec configuration (`av1C`) from the bitstream.
//!
//! GStreamer's `av1parse` doesn't put an `av1C` record in its caps, so the MP4 writer gets
//! it from the first keyframe: find the sequence header OBU (AV1 spec 5.3), read the few
//! fields `av1C` repeats (5.5: profile, level and tier of operating point 0, colour
//! config), and append the OBU itself as `configOBUs` (AV1-ISOBMFF 2.3).
//!
//! Input is untrusted encoder output: every read is bounds-checked and a malformed stream
//! gives `None`.

/// OBU type of a sequence header.
const OBU_SEQUENCE_HEADER: u8 = 1;

/// Largest sequence header accepted (real ones are tens of bytes).
const MAX_SEQUENCE_HEADER: usize = 1024;

/// The fields of a sequence header that `av1C` carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SequenceHeader {
    pub seq_profile: u8,
    pub seq_level_idx_0: u8,
    pub seq_tier_0: u8,
    pub high_bitdepth: bool,
    pub twelve_bit: bool,
    pub mono_chrome: bool,
    pub subsampling_x: bool,
    pub subsampling_y: bool,
    pub chroma_sample_position: u8,
}

/// MSB-first bit reader.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn bit(&mut self) -> Option<u32> {
        let byte = *self.data.get(self.pos.checked_div(8)?)?;
        let shift = 7u32.checked_sub(u32::try_from(self.pos.checked_rem(8)?).ok()?)?;
        self.pos = self.pos.checked_add(1)?;
        Some(u32::from(byte.checked_shr(shift)? & 1))
    }

    /// `f(n)`, n ≤ 32.
    fn f(&mut self, n: u32) -> Option<u32> {
        if n > 32 {
            return None;
        }
        let mut v: u32 = 0;
        for _ in 0..n {
            v = v.checked_shl(1)?.checked_add(self.bit()?)?;
        }
        Some(v)
    }

    fn flag(&mut self) -> Option<bool> {
        Some(self.bit()? == 1)
    }

    /// `uvlc()` (spec 4.10.3); values that don't fit 32 bits are rejected.
    fn uvlc(&mut self) -> Option<u32> {
        let mut leading = 0u32;
        while self.bit()? == 0 {
            leading = leading.checked_add(1)?;
            if leading >= 32 {
                return None;
            }
        }
        let value = self.f(leading)?;
        let base = 1u32.checked_shl(leading)?.checked_sub(1)?;
        value.checked_add(base)
    }
}

/// `leb128()` at the start of `data`: (value, bytes used).
fn leb128(data: &[u8]) -> Option<(u64, usize)> {
    let mut value: u64 = 0;
    for (i, b) in data.iter().take(8).enumerate() {
        let part = u64::from(b & 0x7f).checked_shl(u32::try_from(i.checked_mul(7)?).ok()?)?;
        value |= part;
        if b & 0x80 == 0 {
            return Some((value, i.checked_add(1)?));
        }
    }
    None
}

/// The first sequence header OBU in `data` (a temporal unit in low-overhead OBU format),
/// returned whole (header + size field + payload), and its payload.
pub fn find_sequence_header(data: &[u8]) -> Option<(&[u8], &[u8])> {
    let mut pos = 0usize;
    while pos < data.len() {
        let header = *data.get(pos)?;
        if header & 0x80 != 0 {
            return None; // forbidden bit
        }
        let obu_type = (header >> 3) & 0x0f;
        let has_extension = header & 0x04 != 0;
        let has_size = header & 0x02 != 0;
        if !has_size {
            // Only the last OBU of a unit may omit its size; we can't walk past it.
            return None;
        }
        let size_at = pos
            .checked_add(1)?
            .checked_add(usize::from(has_extension))?;
        let (size, size_len) = leb128(data.get(size_at..)?)?;
        let payload_at = size_at.checked_add(size_len)?;
        let end = payload_at.checked_add(usize::try_from(size).ok()?)?;
        if end > data.len() {
            return None;
        }
        if obu_type == OBU_SEQUENCE_HEADER {
            if end.checked_sub(pos)? > MAX_SEQUENCE_HEADER {
                return None;
            }
            return Some((data.get(pos..end)?, data.get(payload_at..end)?));
        }
        pos = end;
    }
    None
}

/// Parse the fields `av1C` needs from a sequence header OBU payload (spec 5.5).
pub fn parse_sequence_header(payload: &[u8]) -> Option<SequenceHeader> {
    let mut b = Bits {
        data: payload,
        pos: 0,
    };
    let seq_profile = u8::try_from(b.f(3)?).ok()?;
    let _still_picture = b.flag()?;
    let reduced = b.flag()?;
    let seq_level_idx_0;
    let mut seq_tier_0 = 0u8;
    if reduced {
        seq_level_idx_0 = u8::try_from(b.f(5)?).ok()?;
    } else {
        let timing_info_present = b.flag()?;
        let mut decoder_model_info_present = false;
        let mut buffer_delay_length = 0u32;
        if timing_info_present {
            b.f(32)?; // num_units_in_display_tick
            b.f(32)?; // time_scale
            if b.flag()? {
                b.uvlc()?; // num_ticks_per_picture_minus_1
            }
            decoder_model_info_present = b.flag()?;
            if decoder_model_info_present {
                buffer_delay_length = b.f(5)?.checked_add(1)?;
                b.f(32)?; // num_units_in_decoding_tick
                b.f(5)?; // buffer_removal_time_length_minus_1
                b.f(5)?; // frame_presentation_time_length_minus_1
            }
        }
        let initial_display_delay_present = b.flag()?;
        let operating_points = b.f(5)?.checked_add(1)?;
        let mut first: Option<(u8, u8)> = None;
        for _ in 0..operating_points {
            b.f(12)?; // operating_point_idc
            let level = u8::try_from(b.f(5)?).ok()?;
            let tier = if level > 7 {
                u8::try_from(b.f(1)?).ok()?
            } else {
                0
            };
            if decoder_model_info_present && b.flag()? {
                b.f(buffer_delay_length)?; // decoder_buffer_delay
                b.f(buffer_delay_length)?; // encoder_buffer_delay
                b.f(1)?; // low_delay_mode_flag
            }
            if initial_display_delay_present && b.flag()? {
                b.f(4)?; // initial_display_delay_minus_1
            }
            first.get_or_insert((level, tier));
        }
        let (level, tier) = first?;
        seq_level_idx_0 = level;
        seq_tier_0 = tier;
    }
    let width_bits = b.f(4)?.checked_add(1)?;
    let height_bits = b.f(4)?.checked_add(1)?;
    b.f(width_bits)?; // max_frame_width_minus_1
    b.f(height_bits)?; // max_frame_height_minus_1
    let frame_id_numbers_present = if reduced { false } else { b.flag()? };
    if frame_id_numbers_present {
        b.f(4)?; // delta_frame_id_length_minus_2
        b.f(3)?; // additional_frame_id_length_minus_1
    }
    b.f(1)?; // use_128x128_superblock
    b.f(1)?; // enable_filter_intra
    b.f(1)?; // enable_intra_edge_filter
    if !reduced {
        b.f(1)?; // enable_interintra_compound
        b.f(1)?; // enable_masked_compound
        b.f(1)?; // enable_warped_motion
        b.f(1)?; // enable_dual_filter
        let enable_order_hint = b.flag()?;
        if enable_order_hint {
            b.f(1)?; // enable_jnt_comp
            b.f(1)?; // enable_ref_frame_mvs
        }
        let seq_choose_screen_content_tools = b.flag()?;
        let seq_force_screen_content_tools = if seq_choose_screen_content_tools {
            2
        } else {
            b.f(1)?
        };
        if seq_force_screen_content_tools > 0 {
            let seq_choose_integer_mv = b.flag()?;
            if !seq_choose_integer_mv {
                b.f(1)?; // seq_force_integer_mv
            }
        }
        if enable_order_hint {
            b.f(3)?; // order_hint_bits_minus_1
        }
    }
    b.f(1)?; // enable_superres
    b.f(1)?; // enable_cdef
    b.f(1)?; // enable_restoration
    // color_config (5.5.2)
    let high_bitdepth = b.flag()?;
    let twelve_bit = if seq_profile == 2 && high_bitdepth {
        b.flag()?
    } else {
        false
    };
    let mono_chrome = if seq_profile == 1 { false } else { b.flag()? };
    let (mut cp, mut tc, mut mc) = (2u32, 2u32, 2u32); // unspecified
    if b.flag()? {
        cp = b.f(8)?;
        tc = b.f(8)?;
        mc = b.f(8)?;
    }
    let (subsampling_x, subsampling_y, chroma_sample_position) = if mono_chrome {
        b.f(1)?; // color_range
        (true, true, 0)
    } else if cp == 1 && tc == 13 && mc == 0 {
        // sRGB: 4:4:4, full range.
        (false, false, 0)
    } else {
        b.f(1)?; // color_range
        let (x, y) = match seq_profile {
            0 => (true, true),
            1 => (false, false),
            _ if twelve_bit => {
                let x = b.flag()?;
                let y = if x { b.flag()? } else { false };
                (x, y)
            }
            _ => (true, false),
        };
        let csp = if x && y {
            u8::try_from(b.f(2)?).ok()?
        } else {
            0
        };
        (x, y, csp)
    };
    Some(SequenceHeader {
        seq_profile,
        seq_level_idx_0,
        seq_tier_0,
        high_bitdepth,
        twelve_bit,
        mono_chrome,
        subsampling_x,
        subsampling_y,
        chroma_sample_position,
    })
}

/// Build the `av1C` payload (AV1CodecConfigurationRecord) from a temporal unit that
/// contains a sequence header (the first keyframe of a stream).
pub fn config_record(temporal_unit: &[u8]) -> Option<Vec<u8>> {
    let (obu, payload) = find_sequence_header(temporal_unit)?;
    let h = parse_sequence_header(payload)?;
    let mut out = Vec::with_capacity(obu.len().checked_add(4)?);
    out.push(0x81); // marker 1, version 1
    out.push((h.seq_profile & 0x07) << 5 | (h.seq_level_idx_0 & 0x1f));
    out.push(
        (h.seq_tier_0 & 1) << 7
            | u8::from(h.high_bitdepth) << 6
            | u8::from(h.twelve_bit) << 5
            | u8::from(h.mono_chrome) << 4
            | u8::from(h.subsampling_x) << 3
            | u8::from(h.subsampling_y) << 2
            | (h.chroma_sample_position & 0x03),
    );
    out.push(0); // no initial_presentation_delay
    out.extend_from_slice(obu);
    Some(out)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects
    )]

    use super::*;

    /// Write bits MSB first.
    struct W(Vec<u8>, usize);
    impl W {
        fn put(&mut self, v: u32, n: u32) {
            for i in (0..n).rev() {
                if self.1.is_multiple_of(8) {
                    self.0.push(0);
                }
                let bit = ((v >> i) & 1) as u8;
                let last = self.0.len() - 1;
                self.0[last] |= bit << (7 - (self.1 % 8));
                self.1 += 1;
            }
        }
    }

    /// A main-profile 8-bit 4:2:0 sequence header, level 8 (4.0), tier 0, no timing info,
    /// one operating point, 1920x1080, order hints on, BT.709 colour description.
    fn seq_header() -> Vec<u8> {
        let mut w = W(Vec::new(), 0);
        w.put(0, 3); // seq_profile
        w.put(0, 1); // still_picture
        w.put(0, 1); // reduced_still_picture_header
        w.put(0, 1); // timing_info_present_flag
        w.put(0, 1); // initial_display_delay_present_flag
        w.put(0, 5); // operating_points_cnt_minus_1
        w.put(0, 12); // operating_point_idc
        w.put(8, 5); // seq_level_idx (4.0), > 7 so a tier bit follows
        w.put(0, 1); // seq_tier
        w.put(10, 4); // frame_width_bits_minus_1 (11 bits)
        w.put(10, 4); // frame_height_bits_minus_1
        w.put(1919, 11);
        w.put(1079, 11);
        w.put(0, 1); // frame_id_numbers_present_flag
        w.put(0, 3); // 128x128, filter intra, intra edge
        w.put(0, 4); // interintra, masked, warped, dual filter
        w.put(1, 1); // enable_order_hint
        w.put(0, 2); // jnt_comp, ref_frame_mvs
        w.put(1, 1); // seq_choose_screen_content_tools → force = 2
        w.put(1, 1); // seq_choose_integer_mv
        w.put(6, 3); // order_hint_bits_minus_1
        w.put(0, 3); // superres, cdef, restoration
        w.put(0, 1); // high_bitdepth
        w.put(0, 1); // mono_chrome
        w.put(1, 1); // color_description_present_flag
        w.put(1, 8); // BT.709 primaries
        w.put(1, 8); // BT.709 transfer
        w.put(1, 8); // BT.709 matrix
        w.put(0, 1); // color_range
        w.put(1, 2); // chroma_sample_position (colocated)
        w.put(0, 1); // separate_uv_delta_q
        w.put(1, 1); // trailing one bit
        w.0
    }

    fn obu(kind: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![kind << 3 | 0x02, payload.len() as u8];
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn builds_av1c_from_a_keyframe() {
        let sh = seq_header();
        let mut tu = obu(2, &[]); // temporal delimiter
        tu.extend(obu(1, &sh));
        tu.extend(obu(6, &[0x10, 0x20, 0x30])); // frame
        let h = parse_sequence_header(&sh).unwrap();
        assert_eq!(
            h,
            SequenceHeader {
                seq_profile: 0,
                seq_level_idx_0: 8,
                seq_tier_0: 0,
                high_bitdepth: false,
                twelve_bit: false,
                mono_chrome: false,
                subsampling_x: true,
                subsampling_y: true,
                chroma_sample_position: 1,
            }
        );
        let c = config_record(&tu).unwrap();
        assert_eq!(&c[..4], &[0x81, 0x08, 0b0000_1101, 0]);
        assert_eq!(&c[4..], obu(1, &sh).as_slice());
    }

    #[test]
    fn malformed_streams_give_none() {
        let sh = seq_header();
        assert!(config_record(&[]).is_none());
        assert!(config_record(&obu(2, &[])).is_none(), "no sequence header");
        // Truncated payload / size past the end / forbidden bit / no size field.
        assert!(parse_sequence_header(&sh[..3]).is_none());
        let mut bad = obu(1, &sh);
        bad[1] = 200;
        assert!(config_record(&bad).is_none());
        assert!(config_record(&[0x80, 0]).is_none());
        assert!(config_record(&[1 << 3, 0, 0]).is_none());
        // Endless leb128.
        assert!(
            config_record(&[0x0a, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]).is_none()
        );
        // Random bytes never panic.
        let mut x: u32 = 12345;
        for len in 0..200 {
            let data: Vec<u8> = (0..len)
                .map(|_| {
                    x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                    (x >> 16) as u8
                })
                .collect();
            let _ = config_record(&data);
            let _ = parse_sequence_header(&data);
        }
    }

    #[test]
    fn uvlc_and_leb128() {
        assert_eq!(leb128(&[0x05]), Some((5, 1)));
        assert_eq!(leb128(&[0x80, 0x01]), Some((128, 2)));
        let mut b = Bits {
            data: &[0b0010_1000],
            pos: 0,
        };
        // 2 leading zeros, then value 01 → 1 + 3 = 4.
        assert_eq!(b.uvlc(), Some(4));
        let mut zeros = Bits {
            data: &[0; 8],
            pos: 0,
        };
        assert_eq!(zeros.uvlc(), None);
    }
}
