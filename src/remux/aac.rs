//! Análisis de AAC en ADTS (el formato que HLS usa dentro de MPEG-TS).

const SAMPLE_RATES: [u32; 13] =
    [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];

/// Muestras PCM por trama AAC.
pub const SAMPLES_PER_FRAME: u32 = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AacConfig {
    pub object_type: u8,
    pub freq_index: u8,
    pub sample_rate: u32,
    pub channels: u8,
}

impl AacConfig {
    /// AudioSpecificConfig de 2 bytes (para `esds`).
    pub fn audio_specific_config(&self) -> [u8; 2] {
        let v = ((self.object_type as u16) << 11)
            | ((self.freq_index as u16) << 7)
            | ((self.channels as u16) << 3);
        v.to_be_bytes()
    }
}

/// Extrae tramas AAC crudas de un flujo ADTS, tolerando tramas partidas
/// entre llamadas.
#[derive(Default)]
pub struct AdtsParser {
    carry: Vec<u8>,
    pub config: Option<AacConfig>,
}

impl AdtsParser {
    pub fn push(&mut self, data: &[u8], frames: &mut Vec<Vec<u8>>) {
        self.carry.extend_from_slice(data);
        let buf = &self.carry;
        let mut pos = 0;

        while pos + 7 <= buf.len() {
            let h = &buf[pos..];
            let valid = h[0] == 0xFF && h[1] & 0xF6 == 0xF0; // sync + layer 0
            let freq_index = (h[2] >> 2) & 0x0F;
            if !valid || freq_index as usize >= SAMPLE_RATES.len() {
                pos += 1;
                continue;
            }
            let header_len = if h[1] & 1 == 1 { 7 } else { 9 };
            let frame_len = (((h[3] & 3) as usize) << 11) | ((h[4] as usize) << 3) | (h[5] >> 5) as usize;
            if frame_len < header_len {
                pos += 1;
                continue;
            }
            if pos + frame_len > buf.len() {
                break; // trama incompleta: esperar más datos
            }

            if self.config.is_none() {
                let channels = ((h[2] & 1) << 2) | (h[3] >> 6);
                self.config = Some(AacConfig {
                    object_type: ((h[2] >> 6) & 3) + 1,
                    freq_index,
                    sample_rate: SAMPLE_RATES[freq_index as usize],
                    channels: if channels == 0 { 2 } else { channels },
                });
            }
            frames.push(buf[pos + header_len..pos + frame_len].to_vec());
            pos += frame_len;
        }
        self.carry.drain(..pos);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trama ADTS sintética: AAC-LC, 44.1 kHz, estéreo, con `payload` bytes.
    fn frame(payload: usize) -> Vec<u8> {
        let len = 7 + payload;
        let mut f = vec![
            0xFF,
            0xF1,
            (1 << 6) | (4 << 2),
            (2 << 6) | ((len >> 11) & 3) as u8,
            ((len >> 3) & 0xFF) as u8,
            (((len & 7) << 5) as u8) | 0x1F,
            0xFC,
        ];
        f.extend(std::iter::repeat(0xAB).take(payload));
        f
    }

    #[test]
    fn parses_frames_split_across_pushes() {
        let data: Vec<u8> = [frame(10), frame(20)].concat();
        let mut parser = AdtsParser::default();
        let mut frames = Vec::new();
        parser.push(&data[..12], &mut frames);
        assert!(frames.is_empty());
        parser.push(&data[12..], &mut frames);
        assert_eq!(frames.iter().map(Vec::len).collect::<Vec<_>>(), [10, 20]);

        let cfg = parser.config.unwrap();
        assert_eq!((cfg.sample_rate, cfg.channels, cfg.object_type), (44100, 2, 2));
        assert_eq!(cfg.audio_specific_config(), [0x12, 0x10]);
    }
}
