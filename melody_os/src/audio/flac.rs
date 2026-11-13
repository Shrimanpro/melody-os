//! FLAC decoder (integer only). Supports everything the reference encoder produces:
//! CONSTANT / VERBATIM / FIXED / LPC subframes, Rice and Rice2 residuals with escape codes,
//! wasted bits, and left/side, right/side and mid/side stereo. Up to 24 bits per sample.

use alloc::vec;
use alloc::vec::Vec;
use super::{ByteRead, Decoder, StreamInfo};

const EOF: &str = "unexpected end of file";

/// Bit reader over a `ByteRead`, with its own byte buffer. Bit state lives in the decoder
/// between frames (frames always end byte aligned).
struct Bits<'a> {
    buf: &'a mut [u8],
    pos: &'a mut usize,
    len: &'a mut usize,
    src: &'a mut dyn ByteRead,
    acc: u64,
    nbits: u32,
}

impl Bits<'_> {
    #[inline]
    fn byte(&mut self) -> Result<u8, &'static str> {
        if *self.pos == *self.len {
            *self.len = self.src.read(self.buf)?;
            *self.pos = 0;
            if *self.len == 0 {
                return Err(EOF);
            }
        }
        let b = self.buf[*self.pos];
        *self.pos += 1;
        Ok(b)
    }

    #[inline]
    fn read(&mut self, n: u32) -> Result<u32, &'static str> {
        if n == 0 {
            return Ok(0);
        }
        while self.nbits < n {
            self.acc = (self.acc << 8) | self.byte()? as u64;
            self.nbits += 8;
        }
        self.nbits -= n;
        let v = (self.acc >> self.nbits) & ((1u64 << n) - 1);
        self.acc &= (1u64 << self.nbits) - 1;
        Ok(v as u32)
    }

    #[inline]
    fn read_signed(&mut self, n: u32) -> Result<i32, &'static str> {
        if n == 0 {
            return Ok(0);
        }
        let v = self.read(n)?;
        let shift = 32 - n;
        Ok(((v << shift) as i32) >> shift)
    }

    /// Count zero bits up to (and consume) the next 1 bit.
    #[inline]
    fn unary(&mut self) -> Result<u32, &'static str> {
        let mut zeros = 0;
        loop {
            if self.nbits == 0 {
                self.acc = self.byte()? as u64;
                self.nbits = 8;
            }
            if self.acc == 0 {
                zeros += self.nbits;
                self.nbits = 0;
                continue;
            }
            let top = 63 - self.acc.leading_zeros(); // index of the highest set bit
            let lead = self.nbits - 1 - top;
            zeros += lead;
            self.nbits = top;
            self.acc &= (1u64 << self.nbits) - 1;
            return Ok(zeros);
        }
    }

    fn align(&mut self) {
        let drop = self.nbits % 8;
        self.nbits -= drop;
        self.acc &= (1u64 << self.nbits) - 1;
    }

    /// Whole bytes left in the bit cache (after `align`).
    fn take_byte(&mut self) -> Result<u8, &'static str> {
        Ok(self.read(8)? as u8)
    }
}

pub struct FlacDecoder {
    rate: u32,
    channels: u8,
    bps: u8,
    total: u64,
    kbps: u32,
    buf: Vec<u8>,
    pos: usize,
    len: usize,
    acc: u64,
    nbits: u32,
    chans: Vec<Vec<i32>>,
    done: bool,
}

impl FlacDecoder {
    pub fn open(input: &mut dyn ByteRead, file_size: u64) -> Result<Self, &'static str> {
        let mut magic = [0u8; 4];
        input.read_exact(&mut magic)?;
        if &magic[0..3] == b"ID3" {
            // ID3v2 tag in front of the stream: 10-byte header with a syncsafe size.
            let mut rest = [0u8; 6];
            input.read_exact(&mut rest)?;
            let size = rest[2..6].iter().fold(0u64, |a, &b| (a << 7) | (b & 0x7F) as u64);
            input.skip(size)?;
            input.read_exact(&mut magic)?;
        }
        if &magic != b"fLaC" {
            return Err("not a FLAC file");
        }

        let mut info = None;
        loop {
            let mut hdr = [0u8; 4];
            input.read_exact(&mut hdr)?;
            let last = hdr[0] & 0x80 != 0;
            let kind = hdr[0] & 0x7F;
            let len = u32::from_be_bytes([0, hdr[1], hdr[2], hdr[3]]) as u64;
            if kind == 0 {
                if len < 34 {
                    return Err("bad FLAC STREAMINFO");
                }
                let mut si = [0u8; 34];
                input.read_exact(&mut si)?;
                input.skip(len - 34)?;
                let rate = (si[10] as u32) << 12 | (si[11] as u32) << 4 | (si[12] as u32) >> 4;
                let channels = ((si[12] >> 1) & 0x7) + 1;
                let bps = (((si[12] & 1) << 4) | (si[13] >> 4)) + 1;
                let total = ((si[13] & 0xF) as u64) << 32 | u32::from_be_bytes([si[14], si[15], si[16], si[17]]) as u64;
                info = Some((rate, channels, bps, total));
            } else {
                input.skip(len)?;
            }
            if last {
                break;
            }
        }
        let (rate, channels, bps, total) = info.ok_or("FLAC file has no STREAMINFO")?;
        if bps > 24 {
            return Err("FLAC above 24 bits is not supported");
        }
        let secs = if rate > 0 { total / rate as u64 } else { 0 };
        let kbps = if secs > 0 { (file_size * 8 / secs / 1000) as u32 } else { 0 };
        Ok(FlacDecoder {
            rate,
            channels,
            bps,
            total,
            kbps,
            buf: vec![0u8; 16 * 1024],
            pos: 0,
            len: 0,
            acc: 0,
            nbits: 0,
            chans: Vec::new(),
            done: false,
        })
    }

    /// Decode one frame into `self.chans`. Returns the block size, or None at end of stream.
    fn frame(&mut self, input: &mut dyn ByteRead) -> Result<Option<usize>, &'static str> {
        let mut b = Bits { buf: &mut self.buf, pos: &mut self.pos, len: &mut self.len, src: input, acc: self.acc, nbits: self.nbits };
        let res = decode_frame(&mut b, self.rate, self.bps, &mut self.chans);
        self.acc = b.acc;
        self.nbits = b.nbits;
        match res {
            Err(e) if e == EOF => Ok(None),
            other => other.map(Some),
        }
    }
}

fn decode_frame(b: &mut Bits<'_>, stream_rate: u32, stream_bps: u8, chans: &mut Vec<Vec<i32>>) -> Result<usize, &'static str> {
    // Find the frame sync code (0xFFF8 / 0xFFF9). Give up after a while: trailing junk
    // such as an ID3v1 tag means we're done.
    b.align();
    let mut scanned = 0;
    loop {
        let x = b.take_byte()?;
        if x == 0xFF {
            let y = b.take_byte()?;
            if y & 0xFE == 0xF8 {
                break;
            }
        }
        scanned += 1;
        if scanned > 64 * 1024 {
            return Err(EOF);
        }
    }

    let bs_code = b.read(4)?;
    let sr_code = b.read(4)?;
    let ch_code = b.read(4)?;
    let ss_code = b.read(3)?;
    b.read(1)?;
    // UTF-8 style frame/sample number: just skip it.
    let first = b.read(8)?;
    let extra = (first as u8).leading_ones().saturating_sub(1);
    for _ in 0..extra {
        b.read(8)?;
    }
    let block = match bs_code {
        1 => 192,
        2..=5 => 576 << (bs_code - 2),
        6 => b.read(8)? as usize + 1,
        7 => b.read(16)? as usize + 1,
        8..=15 => 256 << (bs_code - 8),
        _ => return Err("reserved FLAC block size"),
    };
    let _rate = match sr_code {
        12 => b.read(8)? * 1000,
        13 => b.read(16)?,
        14 => b.read(16)? * 10,
        15 => return Err("invalid FLAC sample rate"),
        _ => stream_rate,
    };
    let bps = match ss_code {
        0 => stream_bps as u32,
        1 => 8,
        2 => 12,
        4 => 16,
        5 => 20,
        6 => 24,
        _ => return Err("unsupported FLAC sample size"),
    };
    b.read(8)?; // CRC-8

    let nch = match ch_code {
        0..=7 => ch_code as usize + 1,
        8..=10 => 2,
        _ => return Err("reserved FLAC channel assignment"),
    };
    if chans.len() < nch {
        chans.resize_with(nch, Vec::new);
    }
    for (ch, out) in chans.iter_mut().enumerate().take(nch) {
        let side = matches!((ch_code, ch), (8, 1) | (9, 0) | (10, 1));
        out.clear();
        out.resize(block, 0);
        decode_subframe(b, bps + side as u32, out)?;
    }

    // Undo stereo decorrelation.
    if ch_code >= 8 {
        let (a, rest) = chans.split_at_mut(1);
        let (c0, c1) = (&mut a[0], &mut rest[0]);
        for i in 0..block {
            let (x, y) = (c0[i], c1[i]);
            match ch_code {
                8 => c1[i] = x - y,     // left, side -> right
                9 => c0[i] = x + y,     // side, right -> left
                _ => {
                    let mid = (x << 1) | (y & 1);
                    c0[i] = (mid + y) >> 1;
                    c1[i] = (mid - y) >> 1;
                }
            }
        }
    }

    b.align();
    b.read(16)?; // CRC-16
    Ok(block)
}

fn decode_subframe(b: &mut Bits<'_>, mut bps: u32, out: &mut [i32]) -> Result<(), &'static str> {
    if b.read(1)? != 0 {
        return Err("corrupt FLAC subframe");
    }
    let kind = b.read(6)?;
    let wasted = if b.read(1)? == 1 { b.unary()? + 1 } else { 0 };
    bps = bps.checked_sub(wasted).ok_or("corrupt FLAC wasted bits")?;

    match kind {
        0 => {
            let v = b.read_signed(bps)?;
            out.fill(v);
        }
        1 => {
            for s in out.iter_mut() {
                *s = b.read_signed(bps)?;
            }
        }
        8..=12 => {
            let order = (kind - 8) as usize;
            for s in out.iter_mut().take(order) {
                *s = b.read_signed(bps)?;
            }
            residual(b, order, out)?;
            for i in order..out.len() {
                let p: i64 = match order {
                    0 => 0,
                    1 => out[i - 1] as i64,
                    2 => 2 * out[i - 1] as i64 - out[i - 2] as i64,
                    3 => 3 * out[i - 1] as i64 - 3 * out[i - 2] as i64 + out[i - 3] as i64,
                    _ => 4 * out[i - 1] as i64 - 6 * out[i - 2] as i64 + 4 * out[i - 3] as i64 - out[i - 4] as i64,
                };
                out[i] = (out[i] as i64 + p) as i32;
            }
        }
        32..=63 => {
            let order = (kind - 31) as usize;
            for s in out.iter_mut().take(order) {
                *s = b.read_signed(bps)?;
            }
            let precision = b.read(4)? + 1;
            if precision == 16 {
                return Err("corrupt FLAC LPC precision");
            }
            let shift = b.read_signed(5)?;
            if shift < 0 {
                return Err("negative FLAC LPC shift");
            }
            let mut coefs = [0i64; 32];
            for c in coefs.iter_mut().take(order) {
                *c = b.read_signed(precision)? as i64;
            }
            residual(b, order, out)?;
            for i in order..out.len() {
                let mut sum: i64 = 0;
                for (j, c) in coefs.iter().enumerate().take(order) {
                    sum += c * out[i - 1 - j] as i64;
                }
                out[i] = (out[i] as i64 + (sum >> shift)) as i32;
            }
        }
        _ => return Err("reserved FLAC subframe type"),
    }

    if wasted > 0 {
        for s in out.iter_mut() {
            *s <<= wasted;
        }
    }
    Ok(())
}

/// Read the Rice-coded residual into `out[order..]`.
fn residual(b: &mut Bits<'_>, order: usize, out: &mut [i32]) -> Result<(), &'static str> {
    let method = b.read(2)?;
    let (param_bits, escape) = match method {
        0 => (4, 15),
        1 => (5, 31),
        _ => return Err("reserved FLAC residual coding"),
    };
    let porder = b.read(4)?;
    let parts = 1usize << porder;
    let per = out.len() >> porder;
    if per < order || (out.len() & (parts - 1)) != 0 {
        return Err("corrupt FLAC partition order");
    }
    let mut i = order;
    for p in 0..parts {
        let n = if p == 0 { per - order } else { per };
        let param = b.read(param_bits)?;
        if param == escape {
            let raw = b.read(5)?;
            for _ in 0..n {
                out[i] = b.read_signed(raw)?;
                i += 1;
            }
        } else {
            for _ in 0..n {
                let q = b.unary()?;
                let v = (q << param) | b.read(param)?;
                out[i] = ((v >> 1) as i32) ^ -((v & 1) as i32);
                i += 1;
            }
        }
    }
    Ok(())
}

impl Decoder for FlacDecoder {
    fn info(&self) -> StreamInfo {
        StreamInfo { sample_rate: self.rate, bits: self.bps, channels: self.channels, total_frames: self.total, kbps: self.kbps }
    }

    fn codec_name(&self) -> &'static str {
        "FLAC"
    }

    fn decode(&mut self, input: &mut dyn ByteRead, out: &mut Vec<i32>) -> Result<bool, &'static str> {
        if self.done {
            return Ok(false);
        }
        let Some(n) = self.frame(input)? else {
            self.done = true;
            return Ok(false);
        };
        let shift = 32 - self.bps as u32;
        let stereo = self.chans.len() >= 2 && self.channels >= 2;
        out.reserve(n * 2);
        for i in 0..n {
            let l = self.chans[0][i] << shift;
            let r = if stereo { self.chans[1][i] << shift } else { l };
            out.push(l);
            out.push(r);
        }
        Ok(true)
    }
}
