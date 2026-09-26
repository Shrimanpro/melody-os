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
    pub fn parse(bytes: &'a [u8]) -> Result<Self, &'static str> {
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

        // Search for "fmt " chunk
        let mut offset = 12;
        let mut fmt_header = None;
        let mut data_chunk = None;

        while offset + 8 <= bytes.len() {
            let chunk_id = &bytes[offset..offset + 4];
            let chunk_size = u32::from_le_bytes([
                bytes[offset + 4],
                bytes[offset + 5],
                bytes[offset + 6],
                bytes[offset + 7],
            ]) as usize;

            let chunk_data_offset = offset + 8;
            if chunk_data_offset + chunk_size > bytes.len() {
                break;
            }

            if chunk_id == b"fmt " {
                if chunk_size < 16 {
                    return Err("fmt chunk too small");
                }
                let audio_format = u16::from_le_bytes([bytes[chunk_data_offset], bytes[chunk_data_offset + 1]]);
                if audio_format != 1 {
                    // 1 = PCM
                    return Err("Only uncompressed PCM WAV files are supported");
                }
                let num_channels = u16::from_le_bytes([bytes[chunk_data_offset + 2], bytes[chunk_data_offset + 3]]);
                let sample_rate = u32::from_le_bytes([
                    bytes[chunk_data_offset + 4],
                    bytes[chunk_data_offset + 5],
                    bytes[chunk_data_offset + 6],
                    bytes[chunk_data_offset + 7],
                ]);
                let byte_rate = u32::from_le_bytes([
                    bytes[chunk_data_offset + 8],
                    bytes[chunk_data_offset + 9],
                    bytes[chunk_data_offset + 10],
                    bytes[chunk_data_offset + 11],
                ]);
                let block_align = u16::from_le_bytes([bytes[chunk_data_offset + 12], bytes[chunk_data_offset + 13]]);
                let bits_per_sample = u16::from_le_bytes([bytes[chunk_data_offset + 14], bytes[chunk_data_offset + 15]]);

                fmt_header = Some((num_channels, sample_rate, byte_rate, block_align, bits_per_sample));
            } else if chunk_id == b"data" {
                data_chunk = Some((chunk_data_offset, chunk_size));
                break;
            }

            offset = chunk_data_offset + chunk_size;
            // Chunks are 2-byte aligned
            if (offset % 2) != 0 {
                offset += 1;
            }
        }

        let (num_channels, sample_rate, byte_rate, block_align, bits_per_sample) =
            fmt_header.ok_or("Missing fmt chunk in WAV file")?;
        let (data_offset, data_size) =
            data_chunk.ok_or("Missing data chunk in WAV file")?;

        let header = WavHeader {
            num_channels,
            sample_rate,
            byte_rate,
            block_align,
            bits_per_sample,
            data_offset,
            data_size,
        };

        Ok(WavAudio {
            header,
            raw_data: bytes,
        })
    }

    /// Returns a slice containing only the raw PCM sample bytes (RIFF header stripped)
    pub fn samples(&self) -> &'a [u8] {
        let end = self.header.data_offset + self.header.data_size;
        &self.raw_data[self.header.data_offset..end.min(self.raw_data.len())]
    }

    /// Compute HDA stream format bitfield for this audio track
    pub fn hda_format(&self) -> u16 {
        calculate_hda_format(self.header.sample_rate, self.header.num_channels, self.header.bits_per_sample)
    }
}

pub fn calculate_hda_format(sample_rate: u32, num_channels: u16, bits_per_sample: u16) -> u16 {
    // Base rate: 0 for 48 kHz, 1 for 44.1 kHz
    let base_rate_bit = if sample_rate == 44100 { 1u16 << 14 } else { 0u16 };

    // Bits per sample:
    // 000: 8 bits
    // 001: 16 bits
    // 010: 20 bits
    // 011: 24 bits
    // 100: 32 bits
    let bits_val = match bits_per_sample {
        8 => 0,
        16 => 1,
        20 => 2,
        24 => 3,
        32 => 4,
        _ => 1,
    };
    let bits_field = (bits_val as u16) << 4;

    // Number of channels - 1 (e.g. 0 for mono, 1 for stereo)
    let channels_field = (num_channels.saturating_sub(1)) & 0x0F;

    base_rate_bit | bits_field | channels_field
}
