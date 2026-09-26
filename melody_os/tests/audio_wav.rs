#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(melody_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::panic::PanicInfo;
use melody_os::audio::wav::WavAudio;

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    test_main();
    loop {}
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    melody_os::test_panic_handler(info)
}

#[test_case]
fn test_wav_parser_riff_pcm() {
    // Generate a minimal valid 44-byte WAV PCM header + 4 bytes of audio
    let mut wav_bytes = [0u8; 48];
    // RIFF chunk descriptor
    wav_bytes[0..4].copy_from_slice(b"RIFF");
    wav_bytes[4..8].copy_from_slice(&(40u32.to_le_bytes())); // ChunkSize = 48 - 8 = 40
    wav_bytes[8..12].copy_from_slice(b"WAVE");

    // "fmt " sub-chunk
    wav_bytes[12..16].copy_from_slice(b"fmt ");
    wav_bytes[16..20].copy_from_slice(&(16u32.to_le_bytes())); // Subchunk1Size = 16 for PCM
    wav_bytes[20..22].copy_from_slice(&(1u16.to_le_bytes()));  // AudioFormat = 1 (PCM)
    wav_bytes[22..24].copy_from_slice(&(2u16.to_le_bytes()));  // NumChannels = 2 (stereo)
    wav_bytes[24..28].copy_from_slice(&(44100u32.to_le_bytes())); // SampleRate = 44100 Hz
    wav_bytes[28..32].copy_from_slice(&(176400u32.to_le_bytes())); // ByteRate = 44100 * 2 * 2
    wav_bytes[32..34].copy_from_slice(&(4u16.to_le_bytes()));  // BlockAlign = 4
    wav_bytes[34..36].copy_from_slice(&(16u16.to_le_bytes())); // BitsPerSample = 16

    // "data" sub-chunk
    wav_bytes[36..40].copy_from_slice(b"data");
    wav_bytes[40..44].copy_from_slice(&(4u32.to_le_bytes())); // Subchunk2Size = 4 bytes
    wav_bytes[44..48].copy_from_slice(&[0x12, 0x34, 0x56, 0x78]); // 1 stereo 16-bit sample

    let wav = WavAudio::parse(&wav_bytes).expect("Failed to parse valid WAV");
    assert_eq!(wav.header.num_channels, 2);
    assert_eq!(wav.header.sample_rate, 44100);
    assert_eq!(wav.header.bits_per_sample, 16);
    assert_eq!(wav.samples(), &[0x12, 0x34, 0x56, 0x78]);

    // HDA format check: 44.1kHz (bit 14 = 1), 16-bit (bits 6:4 = 001), 2 channels (bits 3:0 = 1)
    let expected_hda_fmt = (1 << 14) | (1 << 4) | 1;
    assert_eq!(wav.hda_format(), expected_hda_fmt);
}
