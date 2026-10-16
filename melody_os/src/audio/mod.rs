//! Audio pipeline: file bytes -> decoder -> 32-bit stereo samples -> packer -> HDA ring.

pub mod hda;
pub mod wav;

use alloc::vec::Vec;
use crate::fs::{BlockDevice, FatVolume, FileReader};

/// Something decoders can pull bytes from.
pub trait ByteRead {
    /// Read up to `buf.len()` bytes; 0 means end of stream.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, &'static str>;

    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), &'static str> {
        let mut done = 0;
        while done < buf.len() {
            let n = self.read(&mut buf[done..])?;
            if n == 0 {
                return Err("unexpected end of file");
            }
            done += n;
        }
        Ok(())
    }

    fn skip(&mut self, mut n: u64) -> Result<(), &'static str> {
        let mut scratch = [0u8; 512];
        while n > 0 {
            let want = n.min(512) as usize;
            let got = self.read(&mut scratch[..want])?;
            if got == 0 {
                return Err("unexpected end of file");
            }
            n -= got as u64;
        }
        Ok(())
    }
}

impl ByteRead for &[u8] {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, &'static str> {
        let n = buf.len().min(self.len());
        buf[..n].copy_from_slice(&self[..n]);
        *self = &self[n..];
        Ok(n)
    }
}

/// A file being played, with a read-ahead buffer so decoders can ask for a few bytes at a
/// time without each request turning into a USB transfer.
pub struct BufferedFile {
    reader: FileReader,
    buf: Vec<u8>,
    start: usize,
    end: usize,
}

const READ_AHEAD: usize = 64 * 1024;

impl BufferedFile {
    pub fn new(first_cluster: u32, size: u32) -> Self {
        BufferedFile { reader: FileReader::new(first_cluster, size), buf: alloc::vec![0u8; READ_AHEAD], start: 0, end: 0 }
    }

    pub fn size(&self) -> u64 {
        self.reader.size
    }

    /// Borrow the file together with the volume and device it lives on.
    pub fn input<'a>(&'a mut self, fs: &'a mut FatVolume, dev: &'a mut dyn BlockDevice) -> Input<'a> {
        Input { file: self, fs, dev }
    }
}

pub struct Input<'a> {
    file: &'a mut BufferedFile,
    fs: &'a mut FatVolume,
    dev: &'a mut dyn BlockDevice,
}

impl Input<'_> {
    pub fn size(&self) -> u64 {
        self.file.reader.size
    }

    /// Current logical position in the file.
    pub fn position(&self) -> u64 {
        self.file.reader.position() - (self.file.end - self.file.start) as u64
    }

    pub fn seek(&mut self, pos: u64) -> Result<(), &'static str> {
        self.file.start = 0;
        self.file.end = 0;
        self.file.reader.seek(self.fs, self.dev, pos)
    }
}

impl ByteRead for Input<'_> {
    fn read(&mut self, out: &mut [u8]) -> Result<usize, &'static str> {
        let f = &mut *self.file;
        if f.start == f.end {
            f.start = 0;
            f.end = f.reader.read(self.fs, self.dev, &mut f.buf)?;
            if f.end == 0 {
                return Ok(0);
            }
        }
        let n = out.len().min(f.end - f.start);
        out[..n].copy_from_slice(&f.buf[f.start..f.start + n]);
        f.start += n;
        Ok(n)
    }
}

/// What a decoder produces, as reported to the UI and MPD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamInfo {
    pub sample_rate: u32,
    pub bits: u8,
    pub channels: u8,
    /// 0 when unknown
    pub total_frames: u64,
    /// Average bitrate of the file in kbit/s (0 when unknown)
    pub kbps: u32,
}

pub trait Decoder: Send {
    fn info(&self) -> StreamInfo;
    fn codec_name(&self) -> &'static str;

    /// Decode the next block, appending interleaved *stereo* samples to `out` as 32-bit
    /// values with the significant bits at the top (a 16-bit sample is `s << 16`).
    /// Returns Ok(false) once the stream is finished.
    fn decode(&mut self, input: &mut dyn ByteRead, out: &mut Vec<i32>) -> Result<bool, &'static str>;
}

/// Converts decoder samples into the byte layout the HDA stream expects.
pub struct Packer {
    pub out_bits: u8,
    /// Keep 1 of every `decimate` frames (simple 2x/4x downsampling, averaged).
    pub decimate: usize,
    /// 0-100 software gain, used only when the codec has no volume amp.
    pub soft_volume: Option<u8>,
}

impl Packer {
    pub fn pack(&self, samples: &[i32], out: &mut Vec<u8>) {
        let d = self.decimate.max(1);
        let frames = samples.len() / 2;
        let gain = self.soft_volume.map(|v| (v.min(100) as i64 * v.min(100) as i64 * 65536) / 10000);
        let mut f = 0;
        while f + d <= frames {
            for ch in 0..2 {
                let mut acc: i64 = 0;
                for k in 0..d {
                    acc += samples[(f + k) * 2 + ch] as i64;
                }
                let mut s = acc / d as i64;
                if let Some(g) = gain {
                    s = s * g >> 16;
                }
                let s = s.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
                if self.out_bits == 16 {
                    out.extend_from_slice(&((s >> 16) as i16).to_le_bytes());
                } else {
                    out.extend_from_slice(&((s as u32) & 0xFFFF_FF00).to_le_bytes());
                }
            }
            f += d;
        }
    }
}

#[test_case]
fn test_packer_16bit_and_decimation() {
    let p = Packer { out_bits: 16, decimate: 1, soft_volume: None };
    let mut out = Vec::new();
    p.pack(&[0x1234_0000, -0x1234_0000], &mut out);
    assert_eq!(out, [0x34, 0x12, 0xCC, 0xED]);

    let p = Packer { out_bits: 16, decimate: 2, soft_volume: None };
    let mut out = Vec::new();
    p.pack(&[0x0100_0000, 0, 0x0300_0000, 0], &mut out);
    assert_eq!(out, [0x00, 0x02, 0x00, 0x00]);
}
