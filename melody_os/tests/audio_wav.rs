#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(melody_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::vec::Vec;
use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use melody_os::audio::wav::{read_header, WavDecoder};
use melody_os::audio::Decoder;

entry_point!(main);

fn main(boot_info: &'static BootInfo) -> ! {
    melody_os::init_with_memory(boot_info);
    test_main();
    melody_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    melody_os::test_panic_handler(info)
}

/// Build a WAV file: optional extra chunk before "data", plain or extensible fmt.
fn make_wav(channels: u16, rate: u32, bits: u16, extensible: bool, samples: &[u8]) -> Vec<u8> {
    let block = channels * bits / 8;
    let mut fmt = Vec::new();
    fmt.extend_from_slice(&(if extensible { 0xFFFEu16 } else { 1 }).to_le_bytes());
    fmt.extend_from_slice(&channels.to_le_bytes());
    fmt.extend_from_slice(&rate.to_le_bytes());
    fmt.extend_from_slice(&(rate * block as u32).to_le_bytes());
    fmt.extend_from_slice(&block.to_le_bytes());
    fmt.extend_from_slice(&bits.to_le_bytes());
    if extensible {
        fmt.extend_from_slice(&22u16.to_le_bytes());
        fmt.extend_from_slice(&bits.to_le_bytes());
        fmt.extend_from_slice(&3u32.to_le_bytes());
        fmt.extend_from_slice(&1u16.to_le_bytes()); // sub-format GUID starts with PCM tag
        fmt.extend_from_slice(&[0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xAA, 0, 0x38, 0x9B, 0x71]);
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF\0\0\0\0WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
    out.extend_from_slice(&fmt);
    out.extend_from_slice(b"LIST\x04\0\0\0junk");
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(samples.len() as u32).to_le_bytes());
    out.extend_from_slice(samples);
    out
}

#[test_case]
fn header_skips_unknown_chunks() {
    let wav = make_wav(2, 44100, 16, false, &[0; 16]);
    let fmt = read_header(&mut &wav[..]).unwrap();
    assert_eq!((fmt.channels, fmt.sample_rate, fmt.bits_per_sample), (2, 44100, 16));
    assert_eq!(fmt.total_frames(), 4);
}

#[test_case]
fn decodes_16bit_stereo() {
    let wav = make_wav(2, 44100, 16, false, &[0x34, 0x12, 0xCC, 0xED]);
    let mut input = &wav[..];
    let mut dec = WavDecoder::open(&mut input).unwrap();
    let mut out = Vec::new();
    dec.decode(&mut input, &mut out).unwrap();
    assert_eq!(out, [0x1234 << 16, -0x1234 << 16]);
}

#[test_case]
fn decodes_24bit_extensible() {
    let wav = make_wav(2, 96000, 24, true, &[0x56, 0x34, 0x12, 0xFF, 0xFF, 0xFF]);
    let mut input = &wav[..];
    let mut dec = WavDecoder::open(&mut input).unwrap();
    assert_eq!(dec.info().sample_rate, 96000);
    let mut out = Vec::new();
    dec.decode(&mut input, &mut out).unwrap();
    assert_eq!(out, [0x1234_5600, -256]);
}

#[test_case]
fn mono_8bit_is_duplicated_to_stereo() {
    let wav = make_wav(1, 22050, 8, false, &[0xFF, 0x00]);
    let mut input = &wav[..];
    let mut dec = WavDecoder::open(&mut input).unwrap();
    let mut out = Vec::new();
    dec.decode(&mut input, &mut out).unwrap();
    assert_eq!(out, [127 << 24, 127 << 24, -128 << 24, -128 << 24]);
}

#[test_case]
fn rejects_compressed_wav() {
    let mut wav = make_wav(2, 44100, 16, false, &[0; 4]);
    wav[20] = 2; // ADPCM
    assert!(read_header(&mut &wav[..]).is_err());
}
