//! Utilidades H.264: separación de NALs (Annex B), conversión a AVCC y SPS.

/// Divide un flujo Annex B en NALs (sin el start code).
pub fn split_nals(data: &[u8]) -> Vec<&[u8]> {
    let mut starts: Vec<(usize, usize)> = Vec::new(); // (posición del 00 00 01, inicio del NAL)
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i + 2] > 1 {
            i += 3;
        } else if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push((i, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }

    let mut nals = Vec::with_capacity(starts.len());
    for (k, &(_, begin)) in starts.iter().enumerate() {
        let end = starts.get(k + 1).map_or(data.len(), |&(code, _)| code);
        let mut nal = &data[begin..end];
        while let Some((&0, rest)) = nal.split_last() {
            nal = rest;
        }
        if !nal.is_empty() {
            nals.push(nal);
        }
    }
    nals
}

/// SPS/PPS vistos en el flujo (se conserva el primero de cada tipo).
#[derive(Default)]
pub struct ParamSets {
    pub sps: Option<Vec<u8>>,
    pub pps: Option<Vec<u8>>,
}

impl ParamSets {
    pub fn ready(&self) -> bool {
        self.sps.is_some() && self.pps.is_some()
    }
}

/// Convierte un access unit Annex B a muestras AVCC (prefijo de 4 bytes).
/// Extrae SPS/PPS y descarta AUD/relleno. Devuelve `(datos, es_idr)`.
pub fn to_avcc(data: &[u8], params: &mut ParamSets) -> (Vec<u8>, bool) {
    let mut out = Vec::with_capacity(data.len() + 16);
    let mut keyframe = false;
    for nal in split_nals(data) {
        match nal[0] & 0x1F {
            7 => {
                params.sps.get_or_insert_with(|| nal.to_vec());
            }
            8 => {
                params.pps.get_or_insert_with(|| nal.to_vec());
            }
            9 | 12 => {}
            kind => {
                keyframe |= kind == 5;
                out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                out.extend_from_slice(nal);
            }
        }
    }
    (out, keyframe)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpsInfo {
    pub width: u32,
    pub height: u32,
    pub chroma_format_idc: u32,
    pub bit_depth_luma_minus8: u32,
    pub bit_depth_chroma_minus8: u32,
}

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    fn bit(&mut self) -> Option<u32> {
        let byte = *self.data.get(self.pos / 8)?;
        let bit = (byte >> (7 - self.pos % 8)) & 1;
        self.pos += 1;
        Some(bit as u32)
    }

    fn bits(&mut self, n: u32) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | self.bit()?;
        }
        Some(v)
    }

    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        Some(((1u64 << zeros) - 1 + self.bits(zeros)? as u64) as u32)
    }

    fn se(&mut self) -> Option<i32> {
        let k = self.ue()?;
        Some(if k % 2 == 1 { k.div_ceil(2) as i32 } else { -((k / 2) as i32) })
    }

    fn skip_scaling_list(&mut self, size: usize) -> Option<()> {
        let (mut last, mut next) = (8i32, 8i32);
        for _ in 0..size {
            if next != 0 {
                next = (last + self.se()? + 256) % 256;
            }
            if next != 0 {
                last = next;
            }
        }
        Some(())
    }
}

/// Quita los bytes de emulación (00 00 03 -> 00 00).
fn unescape(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &b in nal {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

/// Lee dimensiones y formato de croma de un SPS (NAL completo, con cabecera).
pub fn parse_sps(nal: &[u8]) -> Option<SpsInfo> {
    if nal.len() < 4 {
        return None;
    }
    let rbsp = unescape(&nal[1..]);
    let mut r = BitReader { data: &rbsp, pos: 0 };

    let profile = r.bits(8)?;
    r.bits(16)?; // constraint flags + level_idc
    r.ue()?; // seq_parameter_set_id

    let (mut chroma, mut bdl, mut bdc) = (1, 0, 0);
    if matches!(profile, 100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135) {
        chroma = r.ue()?;
        if chroma == 3 {
            r.bit()?; // separate_colour_plane_flag
        }
        bdl = r.ue()?;
        bdc = r.ue()?;
        r.bit()?; // qpprime_y_zero_transform_bypass_flag
        if r.bit()? == 1 {
            for i in 0..if chroma != 3 { 8 } else { 12 } {
                if r.bit()? == 1 {
                    r.skip_scaling_list(if i < 6 { 16 } else { 64 })?;
                }
            }
        }
    }

    r.ue()?; // log2_max_frame_num_minus4
    match r.ue()? {
        0 => {
            r.ue()?;
        }
        1 => {
            r.bit()?;
            r.se()?;
            r.se()?;
            for _ in 0..r.ue()? {
                r.se()?;
            }
        }
        _ => {}
    }
    r.ue()?; // max_num_ref_frames
    r.bit()?; // gaps_in_frame_num_value_allowed_flag
    let width_mbs = r.ue()? + 1;
    let height_map_units = r.ue()? + 1;
    let frame_mbs_only = r.bit()?;
    if frame_mbs_only == 0 {
        r.bit()?; // mb_adaptive_frame_field_flag
    }
    r.bit()?; // direct_8x8_inference_flag

    let (mut left, mut right, mut top, mut bottom) = (0, 0, 0, 0);
    if r.bit()? == 1 {
        left = r.ue()?;
        right = r.ue()?;
        top = r.ue()?;
        bottom = r.ue()?;
    }

    let (unit_x, unit_y) = match chroma {
        0 => (1, 2 - frame_mbs_only),
        1 => (2, 2 * (2 - frame_mbs_only)),
        2 => (2, 2 - frame_mbs_only),
        _ => (1, 2 - frame_mbs_only),
    };
    let width = (width_mbs * 16).checked_sub((left + right) * unit_x)?;
    let height = (height_map_units * 16 * (2 - frame_mbs_only)).checked_sub((top + bottom) * unit_y)?;
    if width == 0 || height == 0 {
        return None;
    }

    Some(SpsInfo {
        width,
        height,
        chroma_format_idc: chroma,
        bit_depth_luma_minus8: bdl,
        bit_depth_chroma_minus8: bdc,
    })
}

/// Contenido de la caja `avcC` (AVCDecoderConfigurationRecord).
pub fn avcc_record(sps: &[u8], pps: &[u8], info: &SpsInfo) -> Vec<u8> {
    let mut v = vec![1, sps[1], sps[2], sps[3], 0xFF, 0xE1];
    v.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    v.extend_from_slice(sps);
    v.push(1);
    v.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    v.extend_from_slice(pps);
    if matches!(sps[1], 100 | 110 | 122 | 144) {
        v.push(0xFC | info.chroma_format_idc as u8);
        v.push(0xF8 | info.bit_depth_luma_minus8 as u8);
        v.push(0xF8 | info.bit_depth_chroma_minus8 as u8);
        v.push(0);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_three_and_four_byte_start_codes() {
        let data = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 9];
        let nals = split_nals(&data);
        assert_eq!(nals, vec![&[0x67, 1, 2][..], &[0x68, 3][..], &[0x65, 9][..]]);
    }

    #[test]
    fn parses_high_profile_sps_640x360() {
        // SPS real de x264 (High@3.0, 640x360 con recorte inferior de 8 px).
        let sps = [
            0x67, 0x64, 0x00, 0x1E, 0xAC, 0xD9, 0x40, 0xA0, 0x2F, 0xF9, 0x70, 0x11, 0x00, 0x00, 0x03, 0x00,
            0x01, 0x00, 0x00, 0x03, 0x00, 0x3C, 0x0F, 0x16, 0x2D, 0x96, 0x00,
        ];
        let info = parse_sps(&sps).expect("SPS válido");
        assert_eq!((info.width, info.height, info.chroma_format_idc), (640, 360, 1));
    }

    #[test]
    fn converts_access_unit_to_avcc() {
        let au = [0, 0, 0, 1, 9, 0xF0, 0, 0, 0, 1, 0x67, 1, 2, 3, 0, 0, 1, 0x68, 4, 0, 0, 1, 0x65, 7, 8];
        let mut params = ParamSets::default();
        let (avcc, key) = to_avcc(&au, &mut params);
        assert!(key && params.ready());
        assert_eq!(avcc, [0, 0, 0, 3, 0x65, 7, 8]);
    }
}
