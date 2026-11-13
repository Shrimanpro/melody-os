//! MP3 (MPEG-1/2 Layer III) via `nanomp3`, a pure-Rust port of minimp3.

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use nanomp3::{DecodeError, MAX_SAMPLES_PER_FRAME};
use super::{ByteRead, Decoder, StreamInfo};

const BUF: usize = 32 * 1024;
/// minimp3 wants several frames of lookahead to recognise a frame reliably.
const LOOKAHEAD: usize = 16 * 1024;

pub struct Mp3Decoder {
    dec: Box<nanomp3::Decoder>,
    buf: Vec<u8>,
    start: usize,
    end: usize,
    input_done: bool,
    pcm: Box<[i16; MAX_SAMPLES_PER_FRAME]>,
    /// Samples decoded while probing the stream in `open`, not yet handed out.
    pending: Option<(usize, u8)>,
    info: StreamInfo,
}

impl Mp3Decoder {
    pub fn open(input: &mut dyn ByteRead, file_size: u64) -> Result<Self, &'static str> {
        let mut d = Mp3Decoder {
            dec: Box::new(nanomp3::Decoder::new()),
            buf: vec![0u8; BUF],
            start: 0,
            end: 0,
            input_done: false,
            pcm: Box::new([0; MAX_SAMPLES_PER_FRAME]),
            pending: None,
            info: StreamInfo { sample_rate: 0, bits: 16, channels: 2, total_frames: 0, kbps: 0 },
        };
        // Skip an ID3v2 tag up front so a large embedded cover image doesn't count as junk.
        d.fill(input)?;
        let tag_len = nanomp3::id3v2_len(&d.buf[..d.end]) as u64;
        let len = tag_len;
        if len > 0 {
            let in_buf = (d.end as u64).min(len);
            d.start = in_buf as usize;
            if len > in_buf {
                input.skip(len - in_buf)?;
            }
        }

        let (n, ch, rate, kbps) = d.next_frame(input)?.ok_or("no MP3 audio frames found")?;
        d.pending = Some((n, ch));
        // Constant-bitrate estimate from the audio part of the file (cover art excluded);
        // VBR files come out roughly right too.
        let audio_bytes = file_size.saturating_sub(tag_len);
        let secs_x1000 = if kbps > 0 { audio_bytes * 8 / kbps as u64 } else { 0 };
        d.info = StreamInfo {
            sample_rate: rate,
            bits: 16,
            channels: ch,
            total_frames: secs_x1000 * rate as u64 / 1000,
            kbps,
        };
        Ok(d)
    }

    fn fill(&mut self, input: &mut dyn ByteRead) -> Result<(), &'static str> {
        if self.start > 0 {
            self.buf.copy_within(self.start..self.end, 0);
            self.end -= self.start;
            self.start = 0;
        }
        while !self.input_done && self.end < BUF {
            let n = input.read(&mut self.buf[self.end..])?;
            if n == 0 {
                self.input_done = true;
            }
            self.end += n;
        }
        Ok(())
    }

    /// Decode the next frame into `self.pcm`: (samples per channel, channels, rate, kbps).
    fn next_frame(&mut self, input: &mut dyn ByteRead) -> Result<Option<(usize, u8, u32, u32)>, &'static str> {
        loop {
            if !self.input_done && self.end - self.start < LOOKAHEAD {
                self.fill(input)?;
            }
            if self.start >= self.end {
                return Ok(None);
            }
            let (used, res) = self.dec.decode(&self.buf[self.start..self.end], &mut self.pcm[..]);
            self.start += used;
            match res {
                Ok(fi) if fi.samples_produced > 0 => {
                    return Ok(Some((fi.samples_produced, fi.channels.num(), fi.sample_rate, fi.bitrate)));
                }
                Ok(_) | Err(DecodeError::ReservoirUnavailable(_)) | Err(DecodeError::Corrupt(_)) => {}
                Err(DecodeError::UnsupportedLayer(_)) => return Err("MP1/MP2 audio is not supported"),
                Err(_) => {
                    if self.input_done {
                        return Ok(None);
                    }
                    if used == 0 {
                        // Buffer full of junk with no frame in it: drop some and keep looking.
                        self.start = (self.start + 1024).min(self.end);
                    }
                }
            }
        }
    }

    fn emit(&self, n: usize, ch: u8, out: &mut Vec<i32>) {
        out.reserve(n * 2);
        if ch == 1 {
            for &s in &self.pcm[..n] {
                let v = (s as i32) << 16;
                out.push(v);
                out.push(v);
            }
        } else {
            for &s in &self.pcm[..n * 2] {
                out.push((s as i32) << 16);
            }
        }
    }
}

impl Decoder for Mp3Decoder {
    fn info(&self) -> StreamInfo {
        self.info
    }

    fn codec_name(&self) -> &'static str {
        "MP3"
    }

    fn decode(&mut self, input: &mut dyn ByteRead, out: &mut Vec<i32>) -> Result<bool, &'static str> {
        if let Some((n, ch)) = self.pending.take() {
            self.emit(n, ch, out);
            return Ok(true);
        }
        // A few frames per call keeps the per-call overhead low.
        for _ in 0..4 {
            match self.next_frame(input)? {
                Some((n, ch, rate, _)) => {
                    if rate != self.info.sample_rate {
                        return Err("MP3 sample rate changes mid-stream");
                    }
                    self.emit(n, ch, out);
                }
                None => return Ok(false),
            }
        }
        Ok(true)
    }
}
