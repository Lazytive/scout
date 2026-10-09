//! Minimal ISO-BMFF box writer: big-endian fields into a growable buffer, sizes patched
//! when a box is finished. Every size computation is checked.

use crate::Mp4Error;

pub(crate) struct BoxBuf {
    pub(crate) buf: Vec<u8>,
}

impl BoxBuf {
    pub(crate) fn new() -> Self {
        BoxBuf { buf: Vec::new() }
    }

    /// Start a plain box; returns its offset for [`BoxBuf::finish`].
    pub(crate) fn start(&mut self, fourcc: &[u8; 4]) -> usize {
        let at = self.buf.len();
        self.buf.extend_from_slice(&[0, 0, 0, 0]);
        self.buf.extend_from_slice(fourcc);
        at
    }

    /// Start a full box (version + 24-bit flags).
    pub(crate) fn start_full(&mut self, fourcc: &[u8; 4], version: u8, flags: u32) -> usize {
        let at = self.start(fourcc);
        self.u8(version);
        self.u24(flags);
        at
    }

    /// Patch the 32-bit size of the box started at `at`.
    pub(crate) fn finish(&mut self, at: usize) -> Result<(), Mp4Error> {
        let size = self
            .buf
            .len()
            .checked_sub(at)
            .ok_or(Mp4Error::Internal("box offset past end"))?;
        let size = u32::try_from(size).map_err(|_| Mp4Error::TooLarge("box over 4 GiB"))?;
        let end = at
            .checked_add(4)
            .ok_or(Mp4Error::Internal("box offset overflow"))?;
        let slot = self
            .buf
            .get_mut(at..end)
            .ok_or(Mp4Error::Internal("box offset out of range"))?;
        slot.copy_from_slice(&size.to_be_bytes());
        Ok(())
    }

    pub(crate) fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    pub(crate) fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub(crate) fn i16(&mut self, v: i16) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub(crate) fn u24(&mut self, v: u32) {
        let b = v.to_be_bytes();
        self.buf
            .extend_from_slice(b.get(1..4).unwrap_or(&[0, 0, 0]));
    }

    pub(crate) fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub(crate) fn i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub(crate) fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub(crate) fn i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub(crate) fn bytes(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }

    pub(crate) fn zeros(&mut self, n: usize) {
        self.buf.resize(self.buf.len().saturating_add(n), 0);
    }

    /// MPEG-4 descriptor (ISO/IEC 14496-1) with a 4-byte expandable size.
    pub(crate) fn descriptor(&mut self, tag: u8, body: &[u8]) -> Result<(), Mp4Error> {
        let len = u32::try_from(body.len()).map_err(|_| Mp4Error::TooLarge("descriptor"))?;
        if len >= 1 << 28 {
            return Err(Mp4Error::TooLarge("descriptor"));
        }
        self.u8(tag);
        // Each byte carries 7 bits; the high bit means "more bytes follow".
        let b = |shift: u32| u8::try_from((len >> shift) & 0x7F).unwrap_or(0);
        self.u8(0x80 | b(21));
        self.u8(0x80 | b(14));
        self.u8(0x80 | b(7));
        self.u8(b(0));
        self.bytes(body);
        Ok(())
    }
}
