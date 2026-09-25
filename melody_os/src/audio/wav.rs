/// RIFF WAV Audio file parser.
#[derive(Debug, Clone, Copy)]
pub struct WavHeader {
    pub num_channels: u16,
    pub sample_rate: u32,
    pub byte_rate: u32,
    pub block_align: u16,
    pub bits_per_sample: u16,
    pub data_offset: usize,
    pub data_size: usize,
}

#[derive(Debug, Clone)]
pub struct WavAudio<'a> {
    pub header: WavHeader,
    pub raw_data: &'a [u8],
}

impl<'a> WavAudio<'a> {
    pub fn validate_riff(bytes: &'a [u8]) -> Result<(), &'static str> {
        if bytes.len() < 44 {
            return Err("WAV data too short (less than 44 bytes)");
        }

        // Check RIFF header
        if &bytes[0..4] != b"RIFF" {
            return Err("Invalid RIFF header");
        }
        if &bytes[8..12] != b"WAVE" {
            return Err("Invalid WAVE format signature");
        }

        Ok(())
    }
}
