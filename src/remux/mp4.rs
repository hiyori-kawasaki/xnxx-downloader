//! Muxer MP4 (ISO BMFF) en Rust puro: H.264 + AAC, con `moov` al inicio
//! (faststart). Las muestras se escriben en streaming y las tablas se
//! construyen en memoria; al final el archivo se reescribe con `moov` delante.

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::aac::{AacConfig, SAMPLES_PER_FRAME};
use crate::error::{Error, Result};

pub const VIDEO_TIMESCALE: u32 = 90_000;
const MOVIE_TIMESCALE: u32 = 1000;
const VIDEO_TRACK: u32 = 1;
const AUDIO_TRACK: u32 = 2;
const MDAT_HEADER: u64 = 16; // size=1 + 'mdat' + largesize de 64 bits
const COPY_BUFFER: usize = 1 << 20;

pub struct VideoConfig {
    pub width: u32,
    pub height: u32,
    /// Contenido de la caja `avcC`.
    pub avcc: Vec<u8>,
}

pub struct VideoSample {
    /// Datos en formato AVCC (NALs con prefijo de longitud).
    pub data: Vec<u8>,
    /// Tiempo de decodificación (90 kHz, relativo al primer frame).
    pub dts: i64,
    /// Desfase de composición `pts - dts` (>= 0).
    pub cto: u32,
    pub sync: bool,
}

#[derive(Default)]
struct Track {
    sizes: Vec<u32>,
    chunks: Vec<(u64, u32)>, // (offset en el archivo, nº de muestras)
    pending: Vec<u8>,
    pending_samples: u32,
    chunk_first_dts: i64,
}

pub struct Mp4Muxer {
    out: BufWriter<File>,
    path: PathBuf,
    pos: u64,
    ftyp: Vec<u8>,
    video: Track,
    audio: Track,
    v_dts: Vec<i64>,
    v_cto: Vec<u32>,
    v_sync: Vec<u32>, // números de muestra (base 1) que son keyframes
    v_max_pts: i64,
    video_cfg: Option<VideoConfig>,
    audio_cfg: Option<AacConfig>,
    audio_delay_ms: u32,
}

impl Mp4Muxer {
    pub fn create(path: &Path) -> io::Result<Self> {
        let mut out = BufWriter::with_capacity(COPY_BUFFER, File::create(path)?);
        let ftyp = boxed(b"ftyp", |b| {
            b.extend_from_slice(b"isom");
            b.u32(512);
            for brand in [b"isom", b"iso2", b"avc1", b"mp41"] {
                b.extend_from_slice(brand);
            }
        });
        out.write_all(&ftyp)?;
        // Cabecera de mdat con tamaño de 64 bits; se rellena al terminar.
        out.write_all(&1u32.to_be_bytes())?;
        out.write_all(b"mdat")?;
        out.write_all(&0u64.to_be_bytes())?;

        Ok(Self {
            out,
            path: path.to_path_buf(),
            pos: ftyp.len() as u64 + MDAT_HEADER,
            ftyp,
            video: Track::default(),
            audio: Track::default(),
            v_dts: Vec::new(),
            v_cto: Vec::new(),
            v_sync: Vec::new(),
            v_max_pts: 0,
            video_cfg: None,
            audio_cfg: None,
            audio_delay_ms: 0,
        })
    }

    pub fn set_video_config(&mut self, cfg: VideoConfig) {
        self.video_cfg = Some(cfg);
    }

    pub fn set_audio_config(&mut self, cfg: AacConfig) {
        self.audio_cfg = Some(cfg);
    }

    /// Retraso del audio respecto al primer frame de video visible.
    pub fn set_audio_delay_ms(&mut self, ms: u32) {
        self.audio_delay_ms = ms;
    }

    pub fn push_video(&mut self, s: VideoSample) -> io::Result<()> {
        if self.video.pending_samples == 0 {
            self.video.chunk_first_dts = s.dts;
        }
        if s.sync {
            self.v_sync.push(self.video.sizes.len() as u32 + 1);
        }
        self.video.sizes.push(s.data.len() as u32);
        self.video.pending.extend_from_slice(&s.data);
        self.video.pending_samples += 1;
        self.v_dts.push(s.dts);
        self.v_cto.push(s.cto);
        self.v_max_pts = self.v_max_pts.max(s.dts + s.cto as i64);

        if s.dts - self.video.chunk_first_dts >= VIDEO_TIMESCALE as i64 {
            self.flush(true)?;
        }
        Ok(())
    }

    pub fn push_audio(&mut self, frame: &[u8]) -> io::Result<()> {
        self.audio.sizes.push(frame.len() as u32);
        self.audio.pending.extend_from_slice(frame);
        self.audio.pending_samples += 1;

        let rate = self.audio_cfg.map_or(48_000, |c| c.sample_rate);
        if self.audio.pending_samples * SAMPLES_PER_FRAME >= rate {
            self.flush(false)?;
        }
        Ok(())
    }

    fn flush(&mut self, video: bool) -> io::Result<()> {
        let track = if video { &mut self.video } else { &mut self.audio };
        if track.pending_samples == 0 {
            return Ok(());
        }
        self.out.write_all(&track.pending)?;
        track.chunks.push((self.pos, track.pending_samples));
        self.pos += track.pending.len() as u64;
        track.pending.clear();
        track.pending_samples = 0;
        Ok(())
    }

    /// Cierra el archivo: parchea `mdat`, construye `moov` y lo reescribe
    /// con `moov` antes de `mdat`.
    pub fn finish(mut self) -> Result<()> {
        self.flush(true)?;
        self.flush(false)?;
        if self.video_cfg.is_none() || self.v_dts.is_empty() {
            return Err(Error::Remux("el flujo no contiene video H.264 decodificable".into()));
        }

        let mdat_start = self.ftyp.len() as u64;
        let mdat_len = self.pos - mdat_start;
        self.out.flush()?;
        let file = self.out.get_mut();
        file.seek(SeekFrom::Start(mdat_start + 8))?;
        file.write_all(&mdat_len.to_be_bytes())?;
        file.flush()?;

        // El tamaño de `moov` no depende de los valores de los offsets, solo de
        // si caben en 32 bits; se decide con el tamaño de la primera pasada.
        let probe = self.moov(0, false).len() as u64;
        let co64 = self.pos + probe > u32::MAX as u64;
        let size = self.moov(0, co64).len() as u64;
        let moov = self.moov(size, co64);
        debug_assert_eq!(moov.len() as u64, size);

        let tmp = PathBuf::from(format!("{}.tmp", self.path.display()));
        let result = (|| -> io::Result<()> {
            let mut src = File::open(&self.path)?;
            src.seek(SeekFrom::Start(mdat_start))?;
            let mut dst = BufWriter::with_capacity(COPY_BUFFER, File::create(&tmp)?);
            dst.write_all(&self.ftyp)?;
            dst.write_all(&moov)?;
            let mut body = BufReader::with_capacity(COPY_BUFFER, src.take(mdat_len));
            io::copy(&mut body, &mut dst)?;
            dst.flush()
        })();
        if let Err(e) = result {
            let _ = fs::remove_file(&tmp);
            return Err(e.into());
        }

        // Windows no permite reemplazar un archivo con un handle abierto.
        let path = self.path.clone();
        drop(self.out);
        fs::rename(&tmp, &path).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            Error::from(e)
        })
    }

    // --- moov ---------------------------------------------------------------

    fn video_durations(&self) -> Vec<u32> {
        let n = self.v_dts.len();
        let mut durs: Vec<u32> = self.v_dts.windows(2).map(|w| (w[1] - w[0]).max(1) as u32).collect();
        durs.push(durs.last().copied().unwrap_or(3000).max(1));
        debug_assert_eq!(durs.len(), n);
        durs
    }

    fn moov(&self, shift: u64, co64: bool) -> Vec<u8> {
        let durs = self.video_durations();
        // La duración de medios debe cubrir hasta el final del último frame
        // *mostrado* (pts máximo + duración), no solo hasta el último DTS.
        let last_dur = *durs.last().unwrap_or(&0) as u64;
        let media = durs
            .iter()
            .map(|&d| d as u64)
            .sum::<u64>()
            .max(self.v_max_pts as u64 + last_dur);
        let cto0 = self.v_cto[0] as u64;
        let video_ms = media.saturating_sub(cto0) * 1000 / VIDEO_TIMESCALE as u64;

        let audio = self.audio_cfg.filter(|_| !self.audio.sizes.is_empty());
        let audio_ms = audio.map_or(0, |c| {
            self.audio.sizes.len() as u64 * SAMPLES_PER_FRAME as u64 * 1000 / c.sample_rate as u64
        });
        let delay = self.audio_delay_ms as u64;
        let movie_ms = video_ms.max(if audio.is_some() { delay + audio_ms } else { 0 });

        let cfg = self.video_cfg.as_ref().expect("comprobado en finish");
        let video_trak = {
            let sample_table = self.video_stbl(cfg, &durs, shift, co64);
            trak(
                VIDEO_TRACK,
                video_ms,
                (cfg.width, cfg.height),
                false,
                &[(video_ms, cto0 as i32)],
                VIDEO_TIMESCALE,
                media,
                b"vide",
                minf_header(true),
                sample_table,
            )
        };

        let audio_trak = audio.map(|c| {
            let edits: Vec<(u64, i32)> = if delay > 0 {
                vec![(delay, -1), (audio_ms, 0)]
            } else {
                vec![(audio_ms, 0)]
            };
            trak(
                AUDIO_TRACK,
                delay + audio_ms,
                (0, 0),
                true,
                &edits,
                c.sample_rate,
                self.audio.sizes.len() as u64 * SAMPLES_PER_FRAME as u64,
                b"soun",
                minf_header(false),
                self.audio_stbl(&c, shift, co64),
            )
        });

        boxed(b"moov", |b| {
            b.extend(full(b"mvhd", 0, 0, |b| {
                b.u32(0);
                b.u32(0);
                b.u32(MOVIE_TIMESCALE);
                b.u32(movie_ms as u32);
                b.u32(0x0001_0000); // rate
                b.u16(0x0100); // volume
                b.zeros(10);
                b.matrix();
                b.zeros(24);
                b.u32(AUDIO_TRACK + 1);
            }));
            b.extend(video_trak);
            if let Some(t) = audio_trak {
                b.extend(t);
            }
        })
    }

    fn video_stbl(&self, cfg: &VideoConfig, durs: &[u32], shift: u64, co64: bool) -> Vec<u8> {
        let stsd = full(b"stsd", 0, 0, |b| {
            b.u32(1);
            b.extend(boxed(b"avc1", |b| {
                b.zeros(6);
                b.u16(1); // data_reference_index
                b.zeros(16);
                b.u16(cfg.width as u16);
                b.u16(cfg.height as u16);
                b.u32(0x0048_0000);
                b.u32(0x0048_0000);
                b.u32(0);
                b.u16(1); // frame_count
                b.zeros(32); // compressorname
                b.u16(0x0018); // depth
                b.u16(0xFFFF); // pre_defined = -1
                b.extend(boxed(b"avcC", |b| b.extend_from_slice(&cfg.avcc)));
            }));
        });

        let stts = table(b"stts", 0, rle(durs.iter().copied()), |b, (n, d)| {
            b.u32(n);
            b.u32(d);
        });
        let ctts = self.v_cto.iter().any(|&c| c != 0).then(|| {
            table(b"ctts", 0, rle(self.v_cto.iter().copied()), |b, (n, o)| {
                b.u32(n);
                b.u32(o);
            })
        });
        let stss = (self.v_sync.len() != self.v_dts.len()).then(|| {
            table(b"stss", 0, self.v_sync.clone(), |b, n| b.u32(n))
        });

        sample_table(stsd, stts, ctts, stss, &self.video, shift, co64)
    }

    fn audio_stbl(&self, cfg: &AacConfig, shift: u64, co64: bool) -> Vec<u8> {
        let asc = cfg.audio_specific_config();
        let stsd = full(b"stsd", 0, 0, |b| {
            b.u32(1);
            b.extend(boxed(b"mp4a", |b| {
                b.zeros(6);
                b.u16(1);
                b.zeros(8);
                b.u16(cfg.channels as u16);
                b.u16(16); // samplesize
                b.u32(0);
                b.u32(cfg.sample_rate << 16);
                b.extend(full(b"esds", 0, 0, |b| {
                    // ES_Descriptor > DecoderConfigDescriptor > DecoderSpecificInfo
                    let dsi = [&[0x05, asc.len() as u8][..], &asc[..]].concat();
                    let mut dcd = vec![0x40, 0x15, 0, 0, 0];
                    dcd.extend_from_slice(&0u32.to_be_bytes()); // maxBitrate
                    dcd.extend_from_slice(&0u32.to_be_bytes()); // avgBitrate
                    dcd.extend_from_slice(&dsi);
                    let sl = [0x06, 0x01, 0x02];
                    let es_len = 3 + 2 + dcd.len() + sl.len();
                    b.extend_from_slice(&[0x03, es_len as u8, 0, 0, 0]);
                    b.extend_from_slice(&[0x04, dcd.len() as u8]);
                    b.extend_from_slice(&dcd);
                    b.extend_from_slice(&sl);
                }));
            }));
        });
        let n = self.audio.sizes.len() as u32;
        let stts = table(b"stts", 0, vec![(n, SAMPLES_PER_FRAME)], |b, (n, d)| {
            b.u32(n);
            b.u32(d);
        });
        sample_table(stsd, stts, None, None, &self.audio, shift, co64)
    }
}

/// stbl común: stsd, stts, [ctts], [stss], stsc, stsz, stco/co64.
fn sample_table(
    stsd: Vec<u8>,
    stts: Vec<u8>,
    ctts: Option<Vec<u8>>,
    stss: Option<Vec<u8>>,
    track: &Track,
    shift: u64,
    co64: bool,
) -> Vec<u8> {
    // stsc: una entrada por cada cambio en el nº de muestras por chunk.
    let mut runs: Vec<(u32, u32)> = Vec::new(); // (primer chunk, muestras por chunk)
    for (i, &(_, n)) in track.chunks.iter().enumerate() {
        if runs.last().map_or(true, |&(_, prev)| prev != n) {
            runs.push((i as u32 + 1, n));
        }
    }
    let stsc = table(b"stsc", 0, runs, |b, (first, n)| {
        b.u32(first);
        b.u32(n);
        b.u32(1);
    });

    let stsz = full(b"stsz", 0, 0, |b| {
        b.u32(0);
        b.u32(track.sizes.len() as u32);
        for &s in &track.sizes {
            b.u32(s);
        }
    });

    let offsets = track.chunks.iter().map(|&(o, _)| o + shift);
    let chunk_offsets = if co64 {
        table(b"co64", 0, offsets.collect(), |b, o| b.u64(o))
    } else {
        table(b"stco", 0, offsets.collect(), |b, o| b.u32(o as u32))
    };

    boxed(b"stbl", |b| {
        b.extend(stsd);
        b.extend(stts);
        b.extend(ctts.into_iter().flatten());
        b.extend(stss.into_iter().flatten());
        b.extend(stsc);
        b.extend(stsz);
        b.extend(chunk_offsets);
    })
}

fn minf_header(video: bool) -> Vec<u8> {
    if video {
        full(b"vmhd", 0, 1, |b| b.zeros(8))
    } else {
        full(b"smhd", 0, 0, |b| b.zeros(4))
    }
}

#[allow(clippy::too_many_arguments)]
fn trak(
    id: u32,
    duration_ms: u64,
    (width, height): (u32, u32),
    audio: bool,
    edits: &[(u64, i32)],
    timescale: u32,
    media_duration: u64,
    handler: &[u8; 4],
    media_header: Vec<u8>,
    stbl: Vec<u8>,
) -> Vec<u8> {
    boxed(b"trak", |b| {
        b.extend(full(b"tkhd", 0, 3, |b| {
            b.u32(0);
            b.u32(0);
            b.u32(id);
            b.u32(0);
            b.u32(duration_ms as u32);
            b.zeros(8);
            b.u16(0); // layer
            b.u16(0); // alternate_group
            b.u16(if audio { 0x0100 } else { 0 });
            b.u16(0);
            b.matrix();
            b.u32(width << 16);
            b.u32(height << 16);
        }));
        b.extend(boxed(b"edts", |b| {
            b.extend(table(b"elst", 0, edits.to_vec(), |b, (dur, media_time)| {
                b.u32(dur as u32);
                b.u32(media_time as u32);
                b.u32(0x0001_0000);
            }));
        }));
        b.extend(boxed(b"mdia", |b| {
            b.extend(full(b"mdhd", 0, 0, |b| {
                b.u32(0);
                b.u32(0);
                b.u32(timescale);
                b.u32(media_duration as u32);
                b.u16(0x55C4); // 'und'
                b.u16(0);
            }));
            b.extend(full(b"hdlr", 0, 0, |b| {
                b.u32(0);
                b.extend_from_slice(handler);
                b.zeros(12);
                b.extend_from_slice(if audio { b"SoundHandler\0" } else { b"VideoHandler\0" });
            }));
            b.extend(boxed(b"minf", |b| {
                b.extend(media_header);
                b.extend(boxed(b"dinf", |b| {
                    b.extend(full(b"dref", 0, 0, |b| {
                        b.u32(1);
                        b.extend(full(b"url ", 0, 1, |_| {}));
                    }));
                }));
                b.extend(stbl);
            }));
        }));
    })
}

// --- helpers de cajas -------------------------------------------------------

trait Put {
    fn u16(&mut self, v: u16);
    fn u32(&mut self, v: u32);
    fn u64(&mut self, v: u64);
    fn zeros(&mut self, n: usize);
    fn matrix(&mut self);
}

impl Put for Vec<u8> {
    fn u16(&mut self, v: u16) {
        self.extend_from_slice(&v.to_be_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.extend_from_slice(&v.to_be_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.extend_from_slice(&v.to_be_bytes());
    }
    fn zeros(&mut self, n: usize) {
        self.resize(self.len() + n, 0);
    }
    fn matrix(&mut self) {
        for v in [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
            self.u32(v);
        }
    }
}

fn boxed(kind: &[u8; 4], body: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut v = vec![0u8; 8];
    v[4..8].copy_from_slice(kind);
    body(&mut v);
    let len = v.len() as u32;
    v[..4].copy_from_slice(&len.to_be_bytes());
    v
}

fn full(kind: &[u8; 4], version: u8, flags: u32, body: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    boxed(kind, |b| {
        b.u32(((version as u32) << 24) | flags);
        body(b);
    })
}

/// Caja de tabla: versión/flags, nº de entradas y cada entrada.
fn table<T>(kind: &[u8; 4], version: u8, rows: Vec<T>, mut row: impl FnMut(&mut Vec<u8>, T)) -> Vec<u8> {
    full(kind, version, 0, |b| {
        b.u32(rows.len() as u32);
        for r in rows {
            row(b, r);
        }
    })
}

/// Codificación por longitud de racha: `[(repeticiones, valor)]`.
fn rle<T: PartialEq + Copy>(values: impl Iterator<Item = T>) -> Vec<(u32, T)> {
    let mut out: Vec<(u32, T)> = Vec::new();
    for v in values {
        match out.last_mut() {
            Some((n, last)) if *last == v => *n += 1,
            _ => out.push((1, v)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rle_groups_runs() {
        assert_eq!(rle([3, 3, 3, 5, 3].into_iter()), vec![(3, 3), (1, 5), (1, 3)]);
    }

    #[test]
    fn box_header_has_total_size() {
        let b = full(b"test", 1, 2, |b| b.u32(7));
        assert_eq!(&b[..4], &16u32.to_be_bytes());
        assert_eq!(&b[4..8], b"test");
        assert_eq!(&b[8..12], &[1, 0, 0, 2]);
    }
}
