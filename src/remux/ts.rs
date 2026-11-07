//! Demuxer MPEG-TS mínimo: PAT/PMT + ensamblado de PES de H.264 y AAC (ADTS).

use std::collections::HashMap;

use crate::error::{Error, Result};

const PACKET: usize = 188;
const WRAP: i64 = 1 << 33;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    Video,
    Audio,
}

/// Un PES completo con marcas de tiempo (90 kHz, ya sin desbordes de 33 bits).
#[derive(Debug)]
pub struct Pes {
    pub kind: StreamKind,
    pub pts: Option<i64>,
    pub dts: Option<i64>,
    pub data: Vec<u8>,
}

#[derive(Default)]
struct Assembler {
    buf: Vec<u8>,
    started: bool,
    last_ts: Option<i64>,
}

#[derive(Default)]
pub struct TsDemuxer {
    buf: Vec<u8>,
    pmt_pid: Option<u16>,
    video_pid: Option<u16>,
    audio_pid: Option<u16>,
    streams: HashMap<u16, Assembler>,
}

impl TsDemuxer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Procesa bytes TS (pueden venir cortados en cualquier punto).
    pub fn push(&mut self, data: &[u8], out: &mut Vec<Pes>) -> Result<()> {
        self.buf.extend_from_slice(data);
        let buf = std::mem::take(&mut self.buf);
        let mut pos = 0;

        while pos + PACKET <= buf.len() {
            // Sincronía: 0x47 y, si se puede comprobar, otro 0x47 un paquete después.
            let synced = buf[pos] == 0x47
                && (pos + PACKET == buf.len() || buf[pos + PACKET] == 0x47);
            if !synced {
                pos += 1;
                continue;
            }
            self.packet(&buf[pos..pos + PACKET], out)?;
            pos += PACKET;
        }

        self.buf = buf;
        self.buf.drain(..pos);
        Ok(())
    }

    /// Emite los PES pendientes al terminar el flujo.
    pub fn finish(&mut self, out: &mut Vec<Pes>) {
        for (&pid, asm) in self.streams.iter_mut() {
            let kind = if Some(pid) == self.video_pid { StreamKind::Video } else { StreamKind::Audio };
            if asm.started {
                flush(asm, kind, out);
            }
        }
    }

    fn packet(&mut self, pkt: &[u8], out: &mut Vec<Pes>) -> Result<()> {
        if pkt[1] & 0x80 != 0 {
            return Ok(()); // transport_error_indicator
        }
        let pusi = pkt[1] & 0x40 != 0;
        let pid = (((pkt[1] & 0x1F) as u16) << 8) | pkt[2] as u16;
        let afc = (pkt[3] >> 4) & 3;
        let mut start = 4;
        if afc & 2 != 0 {
            start += 1 + pkt[4] as usize;
        }
        if afc & 1 == 0 || start >= PACKET {
            return Ok(());
        }
        let payload = &pkt[start..];

        if pid == 0 {
            self.pat(payload, pusi);
        } else if Some(pid) == self.pmt_pid {
            self.pmt(payload, pusi)?;
        } else if Some(pid) == self.video_pid || Some(pid) == self.audio_pid {
            let kind = if Some(pid) == self.video_pid { StreamKind::Video } else { StreamKind::Audio };
            let asm = self.streams.entry(pid).or_default();
            if pusi {
                if asm.started {
                    flush(asm, kind, out);
                }
                asm.buf.clear();
                asm.started = true;
            }
            if asm.started {
                asm.buf.extend_from_slice(payload);
            }
        }
        Ok(())
    }

    fn pat(&mut self, payload: &[u8], pusi: bool) {
        let Some(section) = section(payload, pusi, 0x00) else { return };
        // Tras los 8 bytes de cabecera: (program_number, pid) hasta el CRC.
        for entry in section[8..section.len().saturating_sub(4)].chunks_exact(4) {
            let program = u16::from_be_bytes([entry[0], entry[1]]);
            if program != 0 && self.pmt_pid.is_none() {
                self.pmt_pid = Some(u16::from_be_bytes([entry[2], entry[3]]) & 0x1FFF);
                return;
            }
        }
    }

    fn pmt(&mut self, payload: &[u8], pusi: bool) -> Result<()> {
        let Some(section) = section(payload, pusi, 0x02) else { return Ok(()) };
        if section.len() < 16 {
            return Ok(());
        }
        let program_info = (u16::from_be_bytes([section[10], section[11]]) & 0x0FFF) as usize;
        let mut i = 12 + program_info;
        let end = section.len() - 4;

        while i + 5 <= end {
            let stream_type = section[i];
            let pid = u16::from_be_bytes([section[i + 1], section[i + 2]]) & 0x1FFF;
            let es_info = (u16::from_be_bytes([section[i + 3], section[i + 4]]) & 0x0FFF) as usize;
            i += 5 + es_info;

            match stream_type {
                0x1B => {
                    self.video_pid.get_or_insert(pid);
                }
                0x0F => {
                    self.audio_pid.get_or_insert(pid);
                }
                0x01 | 0x02 | 0x10 | 0x24 | 0x27 => {
                    return Err(Error::UnsupportedCodec(format!(
                        "video (stream_type 0x{stream_type:02X}); solo se admite H.264"
                    )));
                }
                0x03 | 0x04 | 0x11 | 0x81 | 0x87 => {
                    return Err(Error::UnsupportedCodec(format!(
                        "audio (stream_type 0x{stream_type:02X}); solo se admite AAC (ADTS)"
                    )));
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// Devuelve la sección PSI (desde table_id hasta el CRC) si cabe en el paquete.
fn section(payload: &[u8], pusi: bool, table_id: u8) -> Option<&[u8]> {
    if !pusi {
        return None;
    }
    let s = payload.get(1 + *payload.first()? as usize..)?;
    if *s.first()? != table_id || s.len() < 3 {
        return None;
    }
    let len = (((s[1] & 0x0F) as usize) << 8) | s[2] as usize;
    s.get(..3 + len)
}

fn flush(asm: &mut Assembler, kind: StreamKind, out: &mut Vec<Pes>) {
    asm.started = false;
    let b = std::mem::take(&mut asm.buf);
    if b.len() < 9 || b[0] != 0 || b[1] != 0 || b[2] != 1 {
        return;
    }
    let flags = b[7];
    let header_len = b[8] as usize;
    let Some(data) = b.get(9 + header_len..) else { return };

    let mut pts = (flags & 0x80 != 0).then(|| read_ts(&b[9..])).flatten();
    let mut dts = (flags & 0x40 != 0).then(|| read_ts(&b[14..])).flatten();

    for ts in [&mut pts, &mut dts].into_iter().flatten() {
        *ts = unwrap_ts(*ts, asm.last_ts);
    }
    if let Some(ts) = dts.or(pts) {
        asm.last_ts = Some(ts);
    }

    out.push(Pes { kind, pts, dts, data: data.to_vec() });
}

fn read_ts(b: &[u8]) -> Option<i64> {
    let b = b.get(..5)?;
    Some(
        (((b[0] >> 1) & 7) as i64) << 30
            | (b[1] as i64) << 22
            | ((b[2] >> 1) as i64) << 15
            | (b[3] as i64) << 7
            | (b[4] >> 1) as i64,
    )
}

/// Extiende un timestamp de 33 bits a i64 usando el anterior como referencia.
fn unwrap_ts(raw: i64, last: Option<i64>) -> i64 {
    let Some(last) = last else { return raw };
    let mut v = last - last.rem_euclid(WRAP) + raw;
    if v - last > WRAP / 2 {
        v -= WRAP;
    } else if last - v > WRAP / 2 {
        v += WRAP;
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwraps_33_bit_rollover() {
        let last = WRAP - 1000;
        assert_eq!(unwrap_ts(500, Some(last)), WRAP + 500);
        assert_eq!(unwrap_ts(WRAP - 2000, Some(WRAP + 100)), WRAP - 2000);
        assert_eq!(unwrap_ts(1234, None), 1234);
    }

    #[test]
    fn reads_pts() {
        // PTS = 90000 (1 s) codificado en 5 bytes con marcadores.
        let v: i64 = 90000;
        let b = [
            0x21 | (((v >> 30) & 7) << 1) as u8,
            ((v >> 22) & 0xFF) as u8,
            (((v >> 15) & 0x7F) << 1) as u8 | 1,
            ((v >> 7) & 0xFF) as u8,
            ((v & 0x7F) << 1) as u8 | 1,
        ];
        assert_eq!(read_ts(&b), Some(90000));
    }
}
