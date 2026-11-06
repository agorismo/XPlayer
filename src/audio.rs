//! Motor de áudio: thread própria que decodifica (symphonia), aplica ReplayGain + EQ
//! e alimenta um ring buffer consumido pelo callback do cpal.
//!
//! Cadeia (igual à versão Electron): fonte -> ReplayGain -> pré-amp -> EQ 10 bandas -> volume.

use crate::dsp::{EqParams, Equalizer};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering::*};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymErr;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;

pub enum AudioEvent {
    /// A faixa terminou de tocar (buffer esgotado).
    Ended,
    Error(String),
    /// Duração descoberta pelo decodificador (segundos).
    Duration(f64),
}

enum Cmd {
    Load { path: String, play: bool },
    Play,
    Pause,
    Stop,
    Seek(f64),
    Shutdown,
}

struct Shared {
    playing: AtomicBool,
    volume: AtomicU32,  // f32 linear
    rg_gain: AtomicU32, // f32 linear
    pos_frames: AtomicU64,
    base_frames: AtomicU64,
    flush_gen: AtomicU32,
    ack_gen: AtomicU32,
    buffered: AtomicI64, // amostras no ring buffer
    eq: Mutex<EqParams>,
    eq_ver: AtomicU32,
}

pub struct Engine {
    tx: Sender<Cmd>,
    shared: Arc<Shared>,
    pub sample_rate: u32,
}

impl Engine {
    pub fn new() -> Result<(Engine, Receiver<AudioEvent>), String> {
        let shared = Arc::new(Shared {
            playing: AtomicBool::new(false),
            volume: AtomicU32::new(0.64f32.to_bits()),
            rg_gain: AtomicU32::new(1.0f32.to_bits()),
            pos_frames: AtomicU64::new(0),
            base_frames: AtomicU64::new(0),
            flush_gen: AtomicU32::new(0),
            ack_gen: AtomicU32::new(0),
            buffered: AtomicI64::new(0),
            eq: Mutex::new(EqParams::default()),
            eq_ver: AtomicU32::new(1),
        });
        let (tx, rx) = mpsc::channel();
        let (ev_tx, ev_rx) = mpsc::channel();
        let (init_tx, init_rx) = mpsc::channel();
        let sh = shared.clone();
        std::thread::Builder::new()
            .name("audio".into())
            .spawn(move || run(rx, sh, ev_tx, init_tx))
            .map_err(|e| e.to_string())?;
        let rate = init_rx.recv().map_err(|e| e.to_string())??;
        Ok((Engine { tx, shared, sample_rate: rate }, ev_rx))
    }

    pub fn load(&self, path: &str, play: bool) {
        self.shared.playing.store(play, Release); // o estado já vale para quem consultar agora
        let _ = self.tx.send(Cmd::Load { path: path.to_string(), play });
    }
    pub fn play(&self) {
        self.shared.playing.store(true, Release);
        let _ = self.tx.send(Cmd::Play);
    }
    pub fn pause(&self) {
        self.shared.playing.store(false, Release);
        let _ = self.tx.send(Cmd::Pause);
    }
    pub fn stop(&self) {
        self.shared.playing.store(false, Release);
        let _ = self.tx.send(Cmd::Stop);
    }
    pub fn seek(&self, secs: f64) {
        let _ = self.tx.send(Cmd::Seek(secs.max(0.0)));
    }
    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Acquire)
    }
    pub fn position(&self) -> f64 {
        self.shared.pos_frames.load(Relaxed) as f64 / self.sample_rate as f64
    }
    /// 0..=100, curva quadrática como na versão anterior.
    pub fn set_volume(&self, v: f32) {
        let lin = (v / 100.0).clamp(0.0, 1.0).powi(2);
        self.shared.volume.store(lin.to_bits(), Relaxed);
    }
    pub fn set_rg_db(&self, db: f32) {
        self.shared.rg_gain.store(crate::dsp::db_to_lin(db).to_bits(), Relaxed);
    }
    pub fn set_eq(&self, p: EqParams) {
        *self.shared.eq.lock().unwrap() = p;
        self.shared.eq_ver.fetch_add(1, Release);
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Shutdown);
    }
}

// ---------------------------------------------------------------- saída

fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut cons: ringbuf::HeapCons<f32>,
    sh: Arc<Shared>,
) -> Result<cpal::Stream, cpal::BuildStreamError>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let channels = config.channels as usize;
    let mut tmp: Vec<f32> = vec![0.0; 8192];
    let mut my_gen = 0u32;
    device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            let g = sh.flush_gen.load(Acquire);
            if g != my_gen {
                cons.clear();
                sh.buffered.store(0, Release);
                sh.pos_frames.store(sh.base_frames.load(Acquire), Release);
                my_gen = g;
                sh.ack_gen.store(g, Release);
            }
            if data.len() > tmp.len() {
                tmp.resize(data.len(), 0.0);
            }
            let mut got = 0;
            if sh.playing.load(Acquire) {
                got = cons.pop_slice(&mut tmp[..data.len()]);
                sh.buffered.fetch_sub(got as i64, AcqRel);
                sh.pos_frames.fetch_add((got / channels) as u64, AcqRel);
            }
            let vol = f32::from_bits(sh.volume.load(Relaxed));
            for (i, out) in data.iter_mut().enumerate() {
                let v = if i < got { (tmp[i] * vol).clamp(-1.0, 1.0) } else { 0.0 };
                *out = T::from_sample(v);
            }
        },
        |e| eprintln!("[áudio] erro no stream: {e}"),
        None,
    )
}

// ---------------------------------------------------------------- resample linear

struct Resampler {
    step: f64, // frames de entrada por frame de saída
    t: f64,
    prev: Vec<f32>,
    primed: bool,
    ch: usize,
}

impl Resampler {
    fn new(src: u32, dst: u32, ch: usize) -> Self {
        Self { step: src as f64 / dst as f64, t: 0.0, prev: vec![0.0; ch], primed: false, ch }
    }
    fn bypass(&self) -> bool {
        (self.step - 1.0).abs() < 1e-9
    }
    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let ch = self.ch;
        let n = input.len() / ch;
        if n == 0 {
            return;
        }
        if self.bypass() {
            out.extend_from_slice(input);
            return;
        }
        if !self.primed {
            self.prev.copy_from_slice(&input[..ch]);
            self.primed = true;
        }
        // sequência conceitual: [prev, in[0], ..., in[n-1]]  (n+1 frames)
        let at = |i: usize, c: usize| if i == 0 { self.prev[c] } else { input[(i - 1) * ch + c] };
        let mut t = self.t;
        while t + 1.0 <= n as f64 {
            let i = t.floor() as usize;
            let f = (t - i as f64) as f32;
            for c in 0..ch {
                out.push(at(i, c) * (1.0 - f) + at(i + 1, c) * f);
            }
            t += self.step;
        }
        self.t = t - n as f64;
        self.prev.copy_from_slice(&input[(n - 1) * ch..n * ch]);
    }
}

// ---------------------------------------------------------------- decodificação

struct Dec {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    time_base: Option<symphonia::core::units::TimeBase>,
    spec: Option<(u32, usize)>, // (rate, canais) da fonte
    resampler: Option<Resampler>,
    sbuf: Option<SampleBuffer<f32>>,
    eq: Equalizer,
    pending: Vec<f32>,
    poff: usize,
    eof: bool,
}

fn open(path: &str, out_rate: u32, out_ch: usize, ev: &Sender<AudioEvent>) -> Result<Dec, String> {
    let file = File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = Path::new(path).extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions { enable_gapless: true, ..Default::default() }, &MetadataOptions::default())
        .map_err(|e| format!("formato não suportado: {e}"))?;
    let format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or("sem faixa de áudio")?;
    let decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("codec não suportado: {e}"))?;
    if let (Some(n), Some(tb)) = (track.codec_params.n_frames, track.codec_params.time_base) {
        let t = tb.calc_time(n);
        let _ = ev.send(AudioEvent::Duration(t.seconds as f64 + t.frac));
    }
    Ok(Dec {
        track_id: track.id,
        time_base: track.codec_params.time_base,
        format,
        decoder,
        spec: None,
        resampler: None,
        sbuf: None,
        eq: Equalizer::new(out_rate, out_ch),
        pending: Vec::new(),
        poff: 0,
        eof: false,
    })
}

/// Decodifica um pacote e deixa o resultado (já com RG + EQ, na taxa/canais da saída) em `pending`.
fn decode_step(d: &mut Dec, sh: &Shared, out_rate: u32, out_ch: usize, ev: &Sender<AudioEvent>) {
    let packet = match d.format.next_packet() {
        Ok(p) => p,
        Err(SymErr::IoError(_)) | Err(SymErr::ResetRequired) => {
            d.eof = true;
            return;
        }
        Err(e) => {
            let _ = ev.send(AudioEvent::Error(e.to_string()));
            d.eof = true;
            return;
        }
    };
    if packet.track_id() != d.track_id {
        return;
    }
    let decoded = match d.decoder.decode(&packet) {
        Ok(b) => b,
        Err(SymErr::DecodeError(_)) | Err(SymErr::IoError(_)) => return, // pacote ruim: pula
        Err(e) => {
            let _ = ev.send(AudioEvent::Error(e.to_string()));
            d.eof = true;
            return;
        }
    };
    let spec = *decoded.spec();
    let src = (spec.rate, spec.channels.count().max(1));
    if d.spec != Some(src) {
        d.spec = Some(src);
        d.resampler = Some(Resampler::new(src.0, out_rate, out_ch));
        d.sbuf = None;
    }
    let sb = d.sbuf.get_or_insert_with(|| SampleBuffer::<f32>::new(decoded.capacity() as u64, spec));
    if (sb.capacity() as u64) < decoded.frames() as u64 * src.1 as u64 {
        *sb = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
    }
    sb.copy_interleaved_ref(decoded);
    let samples = sb.samples();

    // mapeia canais da fonte -> saída
    let frames = samples.len() / src.1;
    let mut mapped = Vec::with_capacity(frames * out_ch);
    for f in 0..frames {
        let fr = &samples[f * src.1..(f + 1) * src.1];
        for c in 0..out_ch {
            let v = if src.1 == out_ch {
                fr[c]
            } else if src.1 == 1 {
                fr[0]
            } else if out_ch == 1 {
                (fr[0] + fr[1]) * 0.5
            } else if c < src.1 {
                fr[c]
            } else {
                0.0
            };
            mapped.push(v);
        }
    }
    d.pending.clear();
    d.poff = 0;
    d.resampler.as_mut().unwrap().process(&mapped, &mut d.pending);

    let rg = f32::from_bits(sh.rg_gain.load(Relaxed));
    if (rg - 1.0).abs() > 1e-6 {
        for s in d.pending.iter_mut() {
            *s *= rg;
        }
    }
    d.eq.process(&mut d.pending);
}

fn secs(t: Time) -> f64 {
    t.seconds as f64 + t.frac
}

fn run(rx: Receiver<Cmd>, sh: Arc<Shared>, ev: Sender<AudioEvent>, init: Sender<Result<u32, String>>) {
    let host = cpal::default_host();
    let setup = (|| -> Result<(cpal::Stream, ringbuf::HeapProd<f32>, u32, usize), String> {
        let device = host.default_output_device().ok_or("nenhum dispositivo de áudio de saída")?;
        let cfg = device.default_output_config().map_err(|e| e.to_string())?;
        let format = cfg.sample_format();
        let config: cpal::StreamConfig = cfg.into();
        let rate = config.sample_rate.0;
        let ch = config.channels as usize;
        let rb = ringbuf::HeapRb::<f32>::new((rate as usize * ch / 2).max(4096));
        let (prod, cons) = rb.split();
        let stream = match format {
            cpal::SampleFormat::F32 => build_stream::<f32>(&device, &config, cons, sh.clone()),
            cpal::SampleFormat::I16 => build_stream::<i16>(&device, &config, cons, sh.clone()),
            cpal::SampleFormat::U16 => build_stream::<u16>(&device, &config, cons, sh.clone()),
            cpal::SampleFormat::I32 => build_stream::<i32>(&device, &config, cons, sh.clone()),
            f => return Err(format!("formato de amostra não suportado: {f:?}")),
        }
        .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;
        Ok((stream, prod, rate, ch))
    })();
    let (_stream, mut prod, rate, ch) = match setup {
        Ok(v) => {
            let _ = init.send(Ok(v.2));
            v
        }
        Err(e) => {
            let _ = init.send(Err(e));
            return;
        }
    };

    let flush = |base_frames: u64| {
        sh.base_frames.store(base_frames, Release);
        let g = sh.flush_gen.fetch_add(1, AcqRel) + 1;
        for _ in 0..300 {
            if sh.ack_gen.load(Acquire) == g {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    };

    let mut dec: Option<Dec> = None;
    let mut seen_eq = 0u32;
    loop {
        let mut worked = false;

        if let Some(d) = dec.as_mut() {
            let v = sh.eq_ver.load(Acquire);
            if v != seen_eq {
                seen_eq = v;
                d.eq.set_params(*sh.eq.lock().unwrap());
            }
            if d.poff < d.pending.len() {
                let n = prod.push_slice(&d.pending[d.poff..]);
                if n > 0 {
                    sh.buffered.fetch_add(n as i64, AcqRel);
                    d.poff += n;
                    worked = true;
                }
            } else if !d.eof {
                if prod.vacant_len() > prod.capacity().get() / 4 {
                    decode_step(d, &sh, rate, ch, &ev);
                    worked = true;
                }
            } else if sh.buffered.load(Acquire) <= 0 && sh.playing.load(Acquire) {
                dec = None;
                sh.playing.store(false, Release);
                let _ = ev.send(AudioEvent::Ended);
                worked = true;
            }
        }

        let got = if worked {
            rx.try_recv()
        } else {
            rx.recv_timeout(Duration::from_millis(5)).map_err(|e| match e {
                mpsc::RecvTimeoutError::Timeout => mpsc::TryRecvError::Empty,
                mpsc::RecvTimeoutError::Disconnected => mpsc::TryRecvError::Disconnected,
            })
        };
        let cmd = match got {
            Ok(c) => Some(c),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => return, // Engine descartado
        };
        match cmd {
            Some(Cmd::Load { path, play }) => {
                dec = None;
                sh.playing.store(false, Release);
                flush(0);
                match open(&path, rate, ch, &ev) {
                    Ok(mut d) => {
                        d.eq.set_params(*sh.eq.lock().unwrap());
                        seen_eq = sh.eq_ver.load(Acquire);
                        dec = Some(d);
                        sh.playing.store(play, Release);
                    }
                    Err(e) => {
                        let _ = ev.send(AudioEvent::Error(e));
                    }
                }
            }
            Some(Cmd::Play) => {}
            Some(Cmd::Pause) => {}
            Some(Cmd::Stop) => {
                dec = None;
                sh.playing.store(false, Release);
                flush(0);
            }
            Some(Cmd::Seek(s)) => {
                if let Some(d) = dec.as_mut() {
                    let target = Time { seconds: s.floor() as u64, frac: s.fract() };
                    match d.format.seek(SeekMode::Accurate, SeekTo::Time { time: target, track_id: Some(d.track_id) }) {
                        Ok(seeked) => {
                            d.decoder.reset();
                            d.pending.clear();
                            d.poff = 0;
                            d.eof = false;
                            let actual = d.time_base.map(|tb| secs(tb.calc_time(seeked.actual_ts))).unwrap_or(s);
                            if let Some(r) = d.resampler.as_mut() {
                                *r = Resampler::new(d.spec.map(|x| x.0).unwrap_or(rate), rate, ch);
                            }
                            flush((actual * rate as f64) as u64);
                        }
                        Err(e) => {
                            let _ = ev.send(AudioEvent::Error(format!("seek: {e}")));
                        }
                    }
                }
            }
            Some(Cmd::Shutdown) => return,
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MEDIA: &str = "/tmp/claude-0/-home-claude/4d4526df-e6cf-5301-a01d-05b1322b6bac/scratchpad/media";

    fn shared() -> Shared {
        Shared {
            playing: AtomicBool::new(true),
            volume: AtomicU32::new(1.0f32.to_bits()),
            rg_gain: AtomicU32::new(1.0f32.to_bits()),
            pos_frames: AtomicU64::new(0),
            base_frames: AtomicU64::new(0),
            flush_gen: AtomicU32::new(0),
            ack_gen: AtomicU32::new(0),
            buffered: AtomicI64::new(0),
            eq: Mutex::new(EqParams::default()),
            eq_ver: AtomicU32::new(1),
        }
    }

    /// Decodifica o arquivo inteiro; retorna (frames de saída, pico).
    fn decode_all(file: &str, out_rate: u32, out_ch: usize) -> Option<(usize, f32)> {
        let path = format!("{MEDIA}/{file}");
        if !Path::new(&path).exists() {
            eprintln!("(pulando {file}: arquivo de teste ausente)");
            return None;
        }
        let (tx, _rx) = mpsc::channel();
        let sh = shared();
        let mut d = open(&path, out_rate, out_ch, &tx).expect("abrir");
        let mut frames = 0;
        let mut peak = 0f32;
        while !d.eof {
            decode_step(&mut d, &sh, out_rate, out_ch, &tx);
            frames += d.pending.len() / out_ch;
            peak = d.pending.iter().fold(peak, |p, s| p.max(s.abs()));
            d.pending.clear();
        }
        Some((frames, peak))
    }

    #[test]
    fn decodes_formats_and_resamples_to_device_rate() {
        // (arquivo, duração em s)
        for (f, secs) in [("emb.mp3", 3.0), ("plain.flac", 2.0), ("mono.ogg", 2.0), ("m4a_test.m4a", 2.0)] {
            if let Some((frames, peak)) = decode_all(f, 48000, 2) {
                let got = frames as f64 / 48000.0;
                assert!((got - secs).abs() < 0.15, "{f}: duração {got} != {secs}");
                assert!(peak > 0.05 && peak <= 1.2, "{f}: pico {peak}");
            }
        }
    }

    #[test]
    fn mono_is_duplicated_to_stereo_and_surround_device() {
        if let Some((frames, _)) = decode_all("mono.ogg", 44100, 6) {
            assert!((frames as f64 / 44100.0 - 2.0).abs() < 0.15);
        }
    }

    #[test]
    fn seek_lands_near_target() {
        let path = format!("{MEDIA}/emb.mp3");
        if !Path::new(&path).exists() {
            return;
        }
        let (tx, _rx) = mpsc::channel();
        let sh = shared();
        let mut d = open(&path, 44100, 2, &tx).unwrap();
        let target = Time { seconds: 2, frac: 0.0 };
        let s = d.format.seek(SeekMode::Accurate, SeekTo::Time { time: target, track_id: Some(d.track_id) }).unwrap();
        d.decoder.reset();
        let actual = secs(d.time_base.unwrap().calc_time(s.actual_ts));
        assert!((actual - 2.0).abs() < 0.1, "seek caiu em {actual}");
        let mut frames = 0;
        while !d.eof {
            decode_step(&mut d, &sh, 44100, 2, &tx);
            frames += d.pending.len() / 2;
            d.pending.clear();
        }
        assert!((frames as f64 / 44100.0 - 1.0).abs() < 0.15, "sobraram {frames} frames");
    }

    #[test]
    fn resampler_keeps_length_and_dc() {
        let mut r = Resampler::new(44100, 48000, 1);
        let mut out = Vec::new();
        for _ in 0..100 {
            r.process(&vec![0.5f32; 1000], &mut out);
        }
        let expect = 100_000.0 * 48000.0 / 44100.0;
        assert!((out.len() as f64 - expect).abs() < 5.0, "{} vs {}", out.len(), expect);
        assert!(out.iter().all(|s| (s - 0.5).abs() < 1e-5));
    }

    #[test]
    fn replaygain_is_applied_in_pipeline() {
        let path = format!("{MEDIA}/plain.flac");
        if !Path::new(&path).exists() {
            return;
        }
        let (tx, _rx) = mpsc::channel();
        let mut sh = shared();
        let mut d = open(&path, 48000, 2, &tx).unwrap();
        decode_step(&mut d, &sh, 48000, 2, &tx);
        let base = d.pending.iter().fold(0f32, |p, s| p.max(s.abs()));
        let mut d2 = open(&path, 48000, 2, &tx).unwrap();
        sh.rg_gain = AtomicU32::new(crate::dsp::db_to_lin(-6.0).to_bits());
        decode_step(&mut d2, &sh, 48000, 2, &tx);
        let low = d2.pending.iter().fold(0f32, |p, s| p.max(s.abs()));
        assert!((low / base - 0.5012).abs() < 0.02, "{low} / {base}");
    }
}
