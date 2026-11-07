//! Tubería síncrona MPEG-TS -> MP4: demux, conversión de códecs y muxing.

use std::path::Path;

use super::aac::{AdtsParser, SAMPLES_PER_FRAME};
use super::h264::{self, ParamSets};
use super::mp4::{Mp4Muxer, VideoConfig, VideoSample};
use super::ts::{Pes, StreamKind, TsDemuxer};
use crate::error::{Error, Result};

const TS_CLOCK: i64 = 90_000;

pub struct Pipeline {
    demux: TsDemuxer,
    mux: Mp4Muxer,
    scratch: Vec<Pes>,

    // Video
    params: ParamSets,
    /// DTS (absoluto) del primer frame escrito; el video se normaliza a 0.
    v_dts0: Option<i64>,
    /// PTS (absoluto) del primer frame: instante 0 de la línea de tiempo.
    v_pts0: Option<i64>,
    last_dts: i64,

    // Audio
    adts: AdtsParser,
    frames: Vec<Vec<u8>>,
    /// PTS (absoluto) de la primera trama de audio vista.
    a_origin: Option<i64>,
    /// Índice (desde `a_origin`) de la próxima trama a procesar.
    a_index: u64,
    a_waiting: Vec<Vec<u8>>,
    a_started: bool,
}

impl Pipeline {
    pub fn new(output: &Path) -> Result<Self> {
        Ok(Self {
            demux: TsDemuxer::new(),
            mux: Mp4Muxer::create(output)?,
            scratch: Vec::new(),
            params: ParamSets::default(),
            v_dts0: None,
            v_pts0: None,
            last_dts: 0,
            adts: AdtsParser::default(),
            frames: Vec::new(),
            a_origin: None,
            a_index: 0,
            a_waiting: Vec::new(),
            a_started: false,
        })
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<()> {
        let mut pes = std::mem::take(&mut self.scratch);
        let result = self.demux.push(chunk, &mut pes).and_then(|()| self.handle_all(&mut pes));
        self.scratch = pes;
        result
    }

    pub fn finish(mut self) -> Result<()> {
        let mut pes = std::mem::take(&mut self.scratch);
        self.demux.finish(&mut pes);
        self.handle_all(&mut pes)?;
        self.mux.finish()
    }

    fn handle_all(&mut self, pes: &mut Vec<Pes>) -> Result<()> {
        for p in pes.drain(..) {
            match p.kind {
                StreamKind::Video => self.video(&p)?,
                StreamKind::Audio => self.audio(&p)?,
            }
        }
        Ok(())
    }

    fn video(&mut self, p: &Pes) -> Result<()> {
        let Some(dts) = p.dts.or(p.pts) else { return Ok(()) };
        let pts = p.pts.unwrap_or(dts);
        let (data, keyframe) = h264::to_avcc(&p.data, &mut self.params);
        if data.is_empty() {
            return Ok(());
        }

        // Se empieza en el primer IDR con SPS/PPS conocidos.
        if self.v_dts0.is_none() {
            if !(keyframe && self.params.ready()) {
                return Ok(());
            }
            self.start_video(dts, pts)?;
        }

        let base = self.v_dts0.unwrap_or(dts);
        let mut rel = dts - base;
        if rel <= self.last_dts && self.last_dts != 0 {
            rel = self.last_dts + 1; // DTS estrictamente creciente
        }
        self.last_dts = rel;
        let cto = (pts - base - rel).max(0) as u32;

        self.mux.push_video(VideoSample { data, dts: rel, cto, sync: keyframe })?;
        Ok(())
    }

    fn start_video(&mut self, dts: i64, pts: i64) -> Result<()> {
        let (Some(sps), Some(pps)) = (&self.params.sps, &self.params.pps) else {
            return Ok(());
        };
        let info = h264::parse_sps(sps)
            .ok_or_else(|| Error::Remux("no se pudo leer el SPS de H.264".into()))?;
        self.mux.set_video_config(VideoConfig {
            width: info.width,
            height: info.height,
            avcc: h264::avcc_record(sps, pps, &info),
        });
        self.v_dts0 = Some(dts);
        self.v_pts0 = Some(pts);

        // El audio que llegó antes del primer frame ya se puede ubicar.
        for frame in std::mem::take(&mut self.a_waiting) {
            self.place_audio(&frame)?;
        }
        Ok(())
    }

    fn audio(&mut self, p: &Pes) -> Result<()> {
        if self.a_origin.is_none() {
            match p.pts {
                Some(pts) => self.a_origin = Some(pts),
                None => return Ok(()),
            }
        }
        let mut frames = std::mem::take(&mut self.frames);
        self.adts.push(&p.data, &mut frames);
        if let Some(cfg) = self.adts.config {
            self.mux.set_audio_config(cfg);
        }
        for frame in frames.drain(..) {
            if self.v_pts0.is_some() {
                self.place_audio(&frame)?;
            } else {
                self.a_waiting.push(frame);
            }
        }
        self.frames = frames;
        Ok(())
    }

    /// Descarta el audio anterior al primer frame de video y fija el retraso.
    fn place_audio(&mut self, frame: &[u8]) -> Result<()> {
        let (Some(origin), Some(v_pts0), Some(cfg)) = (self.a_origin, self.v_pts0, self.adts.config)
        else {
            return Ok(());
        };
        let pts = origin + (self.a_index * SAMPLES_PER_FRAME as u64 * TS_CLOCK as u64 / cfg.sample_rate as u64) as i64;
        self.a_index += 1;

        if pts < v_pts0 {
            return Ok(());
        }
        if !self.a_started {
            self.a_started = true;
            self.mux.set_audio_delay_ms(((pts - v_pts0) * 1000 / TS_CLOCK) as u32);
        }
        self.mux.push_audio(frame)?;
        Ok(())
    }
}
