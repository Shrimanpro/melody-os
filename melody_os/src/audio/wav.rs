//! Streaming RIFF/WAVE decoder: PCM 8/16/24/32-bit integer and 32-bit float,
//! plain or WAVE_FORMAT_EXTENSIBLE, mono or multichannel (first two channels are played).

use alloc::vec::Vec;
use super::{ByteRead, Decoder, StreamInfo};

const FORMAT_PCM: u16 = 1;
const FORMAT_FLOAT: u16 = 3;
const FORMAT_EXTENSIBLE: u16 = 0xFFFE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WavFormat {
    pub float: bool,
    pub channels: u16,
    pub sample_rate: u32,
    pub bits_per_sample: u16,
    pub block_align: u16,
    pub data_size: u64,
}

impl WavFormat {
    pub fn total_frames(&self) -> u64 {
        if self.block_align == 0 { 0 } else { self.data_size / self.block_align as u64 }
    }
}

fn u16le(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}

fn u32le(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// Read chunks until the "data" chunk; leaves `input` positioned at the first sample.
pub fn read_header(input: &mut dyn ByteRead) -> Result<WavFormat, &'static str> {
    let mut riff = [0u8; 12];
    input.read_exact(&mut riff).map_err(|e| if e == "unexpected end of file" { "file too short for a WAV header" } else { e })?;
    if &riff[0..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file");
    }

    let mut fmt: Option<WavFormat> = None;
    for _ in 0..64 {
        let mut hdr = [0u8; 8];
        input.read_exact(&mut hdr).map_err(|e| if e == "unexpected end of file" { "WAV file has no data chunk" } else { e })?;
        let size = u32le(&hdr[4..8]) as u64;
        match &hdr[0..4] {
            b"fmt " => {
                if !(16..=64).contains(&size) {
                    return Err("malformed fmt chunk");
                }
                let mut b = [0u8; 64];
                input.read_exact(&mut b[..size as usize])?;
                if size % 2 == 1 {
                    input.skip(1)?;
                }
                let mut tag = u16le(&b[0..2]);
                let bits = u16le(&b[14..16]);
                if tag == FORMAT_EXTENSIBLE {
                    if size < 40 {
                        return Err("malformed extensible fmt chunk");
                    }
                    // The sub-format GUID starts with the real format tag.
                    tag = u16le(&b[24..26]);
                }
                let float = match tag {
                    FORMAT_PCM => false,
                    FORMAT_FLOAT if bits == 32 => true,
                    FORMAT_FLOAT => return Err("only 32-bit float WAV is supported"),
                    _ => return Err("compressed WAV (not PCM) is not supported"),
                };
                let channels = u16le(&b[2..4]);
                let block_align = u16le(&b[12..14]);
                if channels == 0 || !matches!(bits, 8 | 16 | 24 | 32) || block_align < channels * (bits / 8) {
                    return Err("unsupported WAV sample layout");
                }
                fmt = Some(WavFormat {
                    float,
                    channels,
                    sample_rate: u32le(&b[4..8]),
                    bits_per_sample: bits,
                    block_align,
                    data_size: 0,
                });
            }
            b"data" => {
                let mut f = fmt.ok_or("data chunk before fmt chunk")?;
                f.data_size = size;
                return Ok(f);
            }
            _ => {
                input.skip(size + (size & 1))?;
            }
        }
    }
    Err("WAV file has no data chunk")
}

pub struct WavDecoder {
    fmt: WavFormat,
    remaining: u64,
    buf: Vec<u8>,
}

/// Frames decoded per call.
const BLOCK_FRAMES: usize = 4096;

impl WavDecoder {
    pub fn open(input: &mut dyn ByteRead) -> Result<Self, &'static str> {
        let fmt = read_header(input)?;
        // Some encoders write 0 or 0xFFFFFFFF for streamed files: play until EOF.
        let remaining = if fmt.data_size == 0 || fmt.data_size == 0xFFFF_FFFF { u64::MAX } else { fmt.data_size };
        Ok(WavDecoder { fmt, remaining, buf: alloc::vec![0u8; BLOCK_FRAMES * fmt.block_align as usize] })
    }

    pub fn format(&self) -> WavFormat {
        self.fmt
    }
}

/// Convert one sample to a 32-bit MSB-aligned integer.
fn sample_to_i32(b: &[u8], bits: u16, float: bool) -> i32 {
    match (bits, float) {
        (8, _) => ((b[0] as i32) - 128) << 24, // 8-bit WAV is unsigned
        (16, _) => (i16::from_le_bytes([b[0], b[1]]) as i32) << 16,
        (24, _) => i32::from_le_bytes([0, b[0], b[1], b[2]]),
        (32, false) => i32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        (32, true) => f32_bits_to_i32(u32le(b)),
        _ => 0,
    }
}

/// IEEE-754 single in [-1, 1] -> i32 full scale, using integer operations only
/// (the kernel is built without an FPU target).
fn f32_bits_to_i32(bits: u32) -> i32 {
    let sign = bits >> 31 != 0;
    let exp = ((bits >> 23) & 0xFF) as i32;
    let mant = (bits & 0x7F_FFFF) | 0x80_0000;
    if exp == 0 {
        return 0; // zero / denormal
    }
    // value = mant * 2^(exp - 127 - 23); we want value * 2^31.
    let shift = exp - 127 - 23 + 31;
    let mag: i64 = if shift >= 0 {
        if shift > 40 { i64::MAX } else { (mant as i64) << shift }
    } else if shift > -32 {
        (mant as i64) >> -shift
    } else {
        0
    };
    let mag = mag.min(i32::MAX as i64);
    if sign { -(mag as i32) } else { mag as i32 }
}

impl Decoder for WavDecoder {
    fn info(&self) -> StreamInfo {
        let f = &self.fmt;
        StreamInfo {
            sample_rate: f.sample_rate,
            bits: f.bits_per_sample as u8,
            channels: f.channels.min(255) as u8,
            total_frames: f.total_frames(),
            kbps: f.sample_rate * f.block_align as u32 * 8 / 1000,
        }
    }

    fn codec_name(&self) -> &'static str {
        "WAV"
    }

    fn decode(&mut self, input: &mut dyn ByteRead, out: &mut Vec<i32>) -> Result<bool, &'static str> {
        if self.remaining == 0 {
            return Ok(false);
        }
        let align = self.fmt.block_align as usize;
        let want = (self.buf.len() as u64).min(self.remaining) as usize / align * align;
        if want == 0 {
            return Ok(false);
        }

        let mut got = 0;
        while got < want {
            let n = input.read(&mut self.buf[got..want])?;
            if n == 0 {
                break;
            }
            got += n;
        }
        let frames = got / align;
        if frames == 0 {
            self.remaining = 0;
            return Ok(false);
        }
        self.remaining = self.remaining.saturating_sub((frames * align) as u64);

        let bytes = (self.fmt.bits_per_sample / 8) as usize;
        let (bits, float, stereo) = (self.fmt.bits_per_sample, self.fmt.float, self.fmt.channels >= 2);
        out.reserve(frames * 2);
        for frame in self.buf[..frames * align].chunks_exact(align) {
            let l = sample_to_i32(&frame[0..bytes], bits, float);
            let r = if stereo { sample_to_i32(&frame[bytes..2 * bytes], bits, float) } else { l };
            out.push(l);
            out.push(r);
        }
        Ok(got == want && self.remaining > 0)
    }
}

#[test_case]
fn test_float_conversion() {
    assert_eq!(f32_bits_to_i32(0x0000_0000), 0); // 0.0
    assert_eq!(f32_bits_to_i32(0x3F00_0000), 1 << 30); // 0.5
    assert_eq!(f32_bits_to_i32(0xBF00_0000), -(1 << 30)); // -0.5
    assert_eq!(f32_bits_to_i32(0x3F80_0000), i32::MAX); // 1.0 clips
}
