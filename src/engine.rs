//! Audio synchronisation engine.
//!
//! Model: every clip has an unknown global time `t` (seconds) at which its media time zero happens.
//! One device (normally the longest audio recorder) is the reference; its clips are laid out by their
//! creation times. Every other device is then matched against the devices already placed using
//! GCC-PHAT cross-correlation (coarse pass over the whole span, then windowed passes constrained by
//! chronology / the device clock, then a fine pass at the full analysis rate). Clips that cannot be
//! matched by audio are placed by their creation time using the clock offset of the nearest synced
//! clip of the same device (and flagged as unsynchronised),.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use rayon::prelude::*;
use realfft::num_complex::Complex32;

use crate::dsp::{decimate, irfft, next_fast_len, rfft};
use crate::media::{Audio, MediaInfo, AUDIO_SR};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Status {
    Reference,
    Synced,
    /// placed by creation time (not audio-matched)
    ByTime,
    /// no information at all, left where it was
    Unsynced,
    Offline,
}

impl Status {
    pub fn code(self) -> &'static str {
        match self {
            Status::Reference => "reference",
            Status::Synced => "synced",
            Status::ByTime => "by-time",
            Status::Unsynced => "unsynced",
            Status::Offline => "offline",
        }
    }
    pub fn pt(self) -> &'static str {
        match self {
            Status::Reference => "referencia",
            Status::Synced => "sincronizado",
            Status::ByTime => "pelo horario",
            Status::Unsynced => "nao sincronizado",
            Status::Offline => "offline",
        }
    }
    pub fn placed(self) -> bool {
        matches!(self, Status::Reference | Status::Synced)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Mode {
    pub name: &'static str,
    pub audio: bool,
    pub coarse_sr: usize,
    pub refine: bool,
    pub clock_window: bool,
    pub extra_srs: &'static [usize],
    pub label: &'static str,
}

pub const MODES: &[Mode] = &[
    Mode { name: "approximate", audio: false, coarse_sr: 0, refine: false, clock_window: true, extra_srs: &[],
           label: "Approximate - so pelo horario de criacao (erro ~1 s)" },
    Mode { name: "very_fast", audio: true, coarse_sr: 1000, refine: false, clock_window: true, extra_srs: &[],
           label: "Very Fast - uma passada grossa" },
    Mode { name: "fast", audio: true, coarse_sr: 1000, refine: true, clock_window: true, extra_srs: &[],
           label: "Fast - duas passadas" },
    Mode { name: "advanced", audio: true, coarse_sr: 2000, refine: true, clock_window: false, extra_srs: &[],
           label: "Advanced - multi-passadas sem confiar no relogio" },
    Mode { name: "optimal", audio: true, coarse_sr: 2000, refine: true, clock_window: true, extra_srs: &[],
           label: "Optimal (padrao) - multi-passadas, usa horario de criacao" },
    Mode { name: "thorough", audio: true, coarse_sr: 2000, refine: true, clock_window: false, extra_srs: &[],
           label: "Thorough - para proxies/arquivos baixados (horario nao confiavel)" },
    Mode { name: "utmost", audio: true, coarse_sr: 2000, refine: true, clock_window: false, extra_srs: &[1000, 4000],
           label: "Utmost - qualidade maxima (varias resolucoes), mais lento" },
];
pub const DEFAULT_MODE: &str = "optimal";

pub fn mode(name: &str) -> Option<&'static Mode> {
    MODES.iter().find(|m| m.name == name)
}

const Z_STRONG: f64 = 12.0; // global-search z-score for an anchor match
const R_STRONG: f64 = 1.35; // anchor must beat the 2nd best peak by this ratio
const Z_LOCAL: f64 = 7.5; // z-score accepted inside a constrained window
const R_LOCAL: f64 = 1.15;
const CLOCK_WINDOW: f64 = 4.0; // +/- seconds around the clock prediction in windowed search
const CHRONO_TOL: f64 = 1.0; // allowed overlap between consecutive clips of one device (s)
const LOW_CUT: f64 = 80.0; // Hz, ignore rumble / hum

#[derive(Clone, Debug)]
pub struct Clip {
    pub key: String,
    pub name: String,
    pub path: String,
    pub info: MediaInfo,
    /// seconds on the raw timeline where media time zero sits
    pub raw_zero: f64,
    /// audio duration (s)
    pub duration: f64,
    pub device: Option<usize>,
    pub order: usize,
    pub t: Option<f64>,
    pub status: Status,
    pub score: f64,
    pub matched_to: String,
}

impl Clip {
    pub fn ctime(&self) -> Option<f64> {
        self.info.ctime
    }
    pub fn end(&self) -> f64 {
        self.t.unwrap_or(0.0) + self.duration
    }
    fn tt(&self) -> f64 {
        self.t.unwrap_or(0.0)
    }
}

#[derive(Clone, Debug)]
pub struct Device {
    pub idx: usize,
    pub name: String,
    pub kind: String, // "camera" | "recorder"
    pub clips: Vec<usize>, // indexes into the clip list, in chronological order
}

#[derive(Clone, Debug)]
struct Candidate {
    t: f64,
    z: f64,
    ratio: f64,
    target: usize, // device index
}

#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Sincronizacao interrompida pelo usuario")
    }
}
impl std::error::Error for Cancelled {}

pub type LogFn = Arc<dyn Fn(&str) + Send + Sync>;
pub type ProgressFn = Arc<dyn Fn(&str, f64) + Send + Sync>;

/// Python-style round (half to even).
pub fn pyround(x: f64) -> f64 {
    x.round_ties_even()
}

fn std_dev(r: &[f32]) -> f64 {
    let n = r.len().max(1) as f64;
    let mean = r.iter().map(|&v| v as f64).sum::<f64>() / n;
    (r.iter().map(|&v| (v as f64 - mean).powi(2)).sum::<f64>() / n).sqrt()
}

fn argmax(r: &[f32]) -> usize {
    let mut bi = 0;
    let mut bv = f32::NEG_INFINITY;
    for (i, &v) in r.iter().enumerate() {
        if v > bv {
            bv = v;
            bi = i;
        }
    }
    bi
}

pub struct SyncEngine<'a> {
    pub clips: &'a mut Vec<Clip>,
    pub devices: &'a mut Vec<Device>,
    load_audio: &'a (dyn Fn(&str) -> Result<Audio> + Sync),
    mode: &'static Mode,
    order: String,
    reference: Option<usize>,
    chronology: bool,
    log: LogFn,
    progress: ProgressFn,
    cancelled: Arc<AtomicBool>,
    audio: HashMap<usize, Audio>,
    rs: HashMap<(usize, usize), Audio>,
    spec: HashMap<(usize, usize), (Arc<Vec<Complex32>>, usize, usize, f64)>,
    clock_ok: HashMap<usize, bool>,
    done: i64,
    total: i64,
}

impl<'a> SyncEngine<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(clips: &'a mut Vec<Clip>, devices: &'a mut Vec<Device>, load_audio: &'a (dyn Fn(&str) -> Result<Audio> + Sync),
               mode_name: &str, order: &str, reference: Option<usize>, chronology: bool, log: LogFn,
               progress: ProgressFn, cancelled: Arc<AtomicBool>) -> Self {
        SyncEngine {
            clips, devices, load_audio,
            mode: mode(mode_name).unwrap_or_else(|| mode(DEFAULT_MODE).unwrap()),
            order: order.to_string(), reference, chronology, log, progress, cancelled,
            audio: HashMap::new(), rs: HashMap::new(), spec: HashMap::new(), clock_ok: HashMap::new(),
            done: 0, total: 1,
        }
    }

    // ------------------------------------------------------------------ helpers
    fn check(&self) -> Result<(), Cancelled> {
        if self.cancelled.load(Ordering::Relaxed) {
            Err(Cancelled)
        } else {
            Ok(())
        }
    }

    fn log(&self, s: &str) {
        (self.log)(s)
    }

    fn get_audio(&mut self, c: usize, sr: usize) -> Audio {
        let a = self.audio[&c].clone();
        if sr == AUDIO_SR {
            return a;
        }
        self.rs.entry((c, sr)).or_insert_with(|| Arc::new(decimate(&a, AUDIO_SR / sr))).clone()
    }

    fn placed(&self, d: usize) -> Vec<usize> {
        self.devices[d].clips.iter().copied().filter(|&c| self.clips[c].status.placed()).collect()
    }

    fn device_span(&self, d: usize) -> (f64, f64) {
        let pl = self.placed(d);
        let t0 = pl.iter().map(|&c| self.clips[c].tt()).fold(f64::INFINITY, f64::min);
        let t1 = pl.iter().map(|&c| self.clips[c].end()).fold(f64::NEG_INFINITY, f64::max);
        (t0, t1)
    }

    fn segment(&mut self, d: usize, t0: f64, t1: f64, sr: usize) -> Vec<f32> {
        let n = (pyround((t1 - t0) * sr as f64) as i64).max(1) as usize;
        let mut buf = vec![0f32; n];
        for c in self.placed(d) {
            let (ct, cend) = (self.clips[c].tt(), self.clips[c].end());
            if cend <= t0 || ct >= t1 || !self.audio.contains_key(&c) {
                continue;
            }
            let a = self.get_audio(c, sr);
            let s = pyround((ct - t0) * sr as f64) as i64;
            let lo = s.max(0);
            let hi = (n as i64).min(s + a.len() as i64);
            if hi > lo {
                buf[lo as usize..hi as usize].copy_from_slice(&a[(lo - s) as usize..(hi - s) as usize]);
            }
        }
        buf
    }

    fn phat(a: &[Complex32], b: &[Complex32], n: usize, sr: usize) -> Vec<f32> {
        let cut = (LOW_CUT * n as f64 / sr as f64) as usize;
        let x: Vec<Complex32> = a.par_iter().zip(b.par_iter()).enumerate().map(|(i, (a, b))| {
            if i < cut {
                return Complex32::new(0.0, 0.0);
            }
            let v = a * b.conj();
            v / (v.norm() + 1e-12)
        }).collect();
        irfft(x, n)
    }

    /// [(index, z)] of the k+1 best peaks separated by >= 0.1 s.
    fn peaks(r: &[f32], sr: usize, k: usize) -> Vec<(usize, f64)> {
        let sd = std_dev(r) + 1e-12;
        let sep = ((0.1 * sr as f64) as usize).max(1);
        let mut work = r.to_vec();
        let mut out = Vec::new();
        for _ in 0..=k {
            let i = argmax(&work);
            let v = work[i];
            if !v.is_finite() || v <= 0.0 {
                break;
            }
            out.push((i, v as f64 / sd));
            let lo = i.saturating_sub(sep);
            let hi = (i + sep + 1).min(work.len());
            for w in &mut work[lo..hi] {
                *w = f32::NEG_INFINITY;
            }
        }
        out
    }

    fn parabolic(r: &[f32], i: usize) -> f64 {
        if i > 0 && i + 1 < r.len() {
            let (a, b, c) = (r[i - 1] as f64, r[i] as f64, r[i + 1] as f64);
            let d = a - 2.0 * b + c;
            if d < 0.0 {
                return i as f64 + 0.5 * (a - c) / d;
            }
        }
        i as f64
    }

    // ------------------------------------------------------------------ search primitives
    fn global_search(&mut self, c: usize, d: usize, sr: usize) -> Vec<Candidate> {
        let x = self.get_audio(c, sr);
        if x.len() < sr / 2 || !x.iter().any(|&v| v != 0.0) {
            return vec![];
        }
        let (t0, t1) = self.device_span(d);
        let key = (d, sr);
        if !self.spec.contains_key(&key) {
            let loaded: Vec<usize> = self.audio.keys().copied().collect();
            let maxlen = loaded.into_iter().map(|k| self.get_audio(k, sr).len()).max().unwrap_or(0);
            let buf = self.segment(d, t0, t1, sr);
            let n = next_fast_len(buf.len() + maxlen);
            let spec = rfft(&buf, n);
            self.spec.insert(key, (Arc::new(spec), n, buf.len(), t0));
        }
        let (a, n, blen, t0) = self.spec[&key].clone();
        let r = Self::phat(&a, &rfft(&x, n), n, sr);
        // keep lags from -(len(x)-min_overlap) to blen-min_overlap
        let ov = x.len().min(sr) / 2;
        let neg_start = n - (x.len() - ov);
        let mut lags: Vec<usize> = (neg_start..n).collect();
        lags.extend(0..(blen as i64 - ov as i64).max(1) as usize);
        let rr: Vec<f32> = lags.iter().map(|&l| r[l]).collect();
        let pk = Self::peaks(&rr, sr, 5);
        let mut out = Vec::new();
        for (j, &(i, z)) in pk.iter().take(5).enumerate() {
            let lag = if lags[i] >= n - x.len() { lags[i] as i64 - n as i64 } else { lags[i] as i64 };
            let ratio = if j == 0 { if pk.len() > 1 { z / pk[1].1 } else { 99.0 } } else { z / pk[0].1 };
            out.push(Candidate { t: t0 + lag as f64 / sr as f64, z, ratio, target: d });
        }
        out
    }

    /// Best placement of clip with t in [lo, hi] against device `d`.
    fn local_search(&mut self, c: usize, d: usize, lo: f64, hi: f64, sr: usize) -> Option<Candidate> {
        let x = self.get_audio(c, sr);
        if x.len() < sr / 4 || !x.iter().any(|&v| v != 0.0) || hi < lo {
            return None;
        }
        let dur = self.clips[c].duration;
        let seg = self.segment(d, lo, hi + dur, sr);
        if !seg.iter().any(|&v| v != 0.0) {
            return None;
        }
        let n = next_fast_len(seg.len() + x.len());
        let r = Self::phat(&rfft(&seg, n), &rfft(&x, n), n, sr);
        let nl = ((pyround((hi - lo) * sr as f64) as i64 + 1).max(1) as usize).min(r.len());
        let rr = &r[..nl];
        if nl < 8 {
            let i = argmax(rr);
            return Some(Candidate { t: lo + Self::parabolic(rr, i) / sr as f64, z: 0.0, ratio: 0.0, target: d });
        }
        let pk = Self::peaks(rr, sr, 2);
        let &(i, z) = pk.first()?;
        let ratio = if pk.len() > 1 { z / pk[1].1 } else { 99.0 };
        Some(Candidate { t: lo + Self::parabolic(rr, i) / sr as f64, z, ratio, target: d })
    }

    fn refine(&mut self, c: usize, d: usize, t: f64, coarse_sr: usize) -> f64 {
        // reverberant rooms give several close PHAT peaks; at a low rate the coarse pass may pick a
        // neighbour, so the fine pass re-decides within +-60 ms at full rate
        let w = (4.0 / coarse_sr as f64).max(0.06);
        match self.local_search(c, d, t - w, t + w, AUDIO_SR) {
            Some(cand) if (cand.t - t).abs() <= w => cand.t, // z inside a tiny window is meaningless
            _ => t,
        }
    }

    // ------------------------------------------------------------------ main flow
    pub fn run(&mut self) -> Result<(), Cancelled> {
        let t_start = Instant::now();
        let all: Vec<usize> = self.devices.iter().flat_map(|d| d.clips.iter().copied()).collect();
        // 1. read audio (in parallel: every file is an ffmpeg process)
        if self.mode.audio {
            self.check()?;
            (self.progress)("Lendo arquivos", 0.0);
            let done = AtomicUsize::new(0);
            let n = all.len().max(1);
            let jobs: Vec<(usize, String, String, bool)> = all.iter()
                .map(|&c| (c, self.clips[c].path.clone(), self.clips[c].name.clone(), self.clips[c].info.exists)).collect();
            let loader = self.load_audio;
            let cancelled = self.cancelled.clone();
            let progress = self.progress.clone();
            let log = self.log.clone();
            let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build().unwrap();
            let results: Vec<(usize, Option<Audio>)> = pool.install(|| {
                jobs.par_iter().map(|(c, path, name, exists)| {
                    if !exists || cancelled.load(Ordering::Relaxed) {
                        return (*c, None);
                    }
                    let r = match loader(path) {
                        Ok(a) => Some(a),
                        Err(e) => {
                            log(&format!("[erro] nao foi possivel ler {name}: {e}"));
                            None
                        }
                    };
                    let k = done.fetch_add(1, Ordering::Relaxed) + 1;
                    progress("Lendo arquivos", k as f64 / n as f64);
                    (*c, r)
                }).collect()
            });
            self.check()?;
            for (c, a) in results {
                if !self.clips[c].info.exists {
                    self.clips[c].status = Status::Offline;
                    continue;
                }
                if let Some(a) = a {
                    self.clips[c].duration = a.len() as f64 / AUDIO_SR as f64;
                    self.audio.insert(c, a);
                }
            }
            (self.progress)("Lendo arquivos", 1.0);
        }
        for &c in &all {
            if self.clips[c].duration == 0.0 {
                self.clips[c].duration = self.clips[c].info.duration.unwrap_or(0.0);
            }
        }

        // 2. per-device chronological order
        for d in 0..self.devices.len() {
            let key = if self.order == "name" || self.order == "xml" { self.order.clone() } else { "ctime".into() };
            self.sort_device(d, &key);
        }

        // 3. reference
        let r = self.pick_reference();
        let total_min = self.total_of(r) / 60.0;
        self.log(&format!("Referencia: {} ({} clipes, {:.1} min)", self.devices[r].name, self.devices[r].clips.len(), total_min));
        let rclips = self.devices[r].clips.clone();
        let have_ct = rclips.iter().all(|&c| self.clips[c].ctime().is_some());
        let base = if have_ct { rclips.iter().filter_map(|&c| self.clips[c].ctime()).fold(f64::INFINITY, f64::min) } else { 0.0 };
        let base = if base.is_finite() { base } else { 0.0 };
        for &c in &rclips {
            let cl = &mut self.clips[c];
            if cl.status == Status::Offline {
                continue;
            }
            cl.t = Some(if have_ct { cl.ctime().unwrap() - base } else { cl.raw_zero });
            cl.status = Status::Reference;
        }
        if !have_ct && self.order != "xml" {
            self.log("Referencia sem horario de criacao: mantendo posicoes do XML");
        }

        let mut others: Vec<usize> = (0..self.devices.len()).filter(|&d| d != r).collect();
        others.sort_by(|&a, &b| self.total_of(b).total_cmp(&self.total_of(a)));
        let mut placed = vec![r];
        self.total = others.iter().map(|&d| self.devices[d].clips.len() as i64).sum::<i64>().max(1);
        self.done = 0;
        for &d in &others {
            self.check()?;
            self.sync_device(d, &placed, false)?;
            placed.push(d);
        }

        // 4. second sweep: unsynced clips against every other placed device
        if self.mode.audio && others.len() > 1 {
            for &d in &others {
                self.spec.clear();
                let pending = self.devices[d].clips.iter().any(|&c| !matches!(self.clips[c].status, Status::Synced | Status::Offline));
                if pending {
                    let tg: Vec<usize> = placed.iter().copied().filter(|&p| p != d).collect();
                    self.sync_device(d, &tg, true)?;
                }
            }
        }
        (self.progress)("Sincronizando", 1.0);

        let n_sync = all.iter().filter(|&&c| self.clips[c].status.placed()).count();
        self.log(&format!("Concluido em {:.1}s: {}/{} clipes sincronizados pelo audio",
                          t_start.elapsed().as_secs_f64(), n_sync, all.len()));
        Ok(())
    }

    fn total_of(&self, d: usize) -> f64 {
        self.devices[d].clips.iter().map(|&c| self.clips[c].duration).sum()
    }

    fn sort_device(&mut self, d: usize, key: &str) {
        let clips = &*self.clips;
        let mut v = self.devices[d].clips.clone();
        match key {
            "name" => v.sort_by_key(|&c| clips[c].name.to_lowercase()),
            "xml" => v.sort_by(|&a, &b| clips[a].raw_zero.total_cmp(&clips[b].raw_zero)),
            _ => v.sort_by(|&a, &b| {
                let ka = clips[a].ctime().unwrap_or(f64::INFINITY);
                let kb = clips[b].ctime().unwrap_or(f64::INFINITY);
                ka.total_cmp(&kb).then(clips[a].raw_zero.total_cmp(&clips[b].raw_zero))
            }),
        }
        for (i, &c) in v.iter().enumerate() {
            self.clips[c].order = i;
        }
        self.devices[d].clips = v;
    }

    fn pick_reference(&self) -> usize {
        if let Some(r) = self.reference {
            if let Some(d) = self.devices.iter().position(|d| d.idx == r) {
                return d;
            }
        }
        let recs: Vec<usize> = (0..self.devices.len()).filter(|&d| self.devices[d].kind == "recorder").collect();
        let pool = if recs.is_empty() { (0..self.devices.len()).collect() } else { recs };
        let mut best = pool[0];
        for &d in &pool[1..] {
            if self.total_of(d) > self.total_of(best) {
                best = d;
            }
        }
        best
    }

    // ------------------------------------------------------------------ device sync
    fn neighbors(&self, d: usize, c: usize) -> (Option<usize>, Option<usize>) {
        let clips = &self.devices[d].clips;
        let o = self.clips[c].order;
        let prev = clips[..o].iter().rev().copied().find(|&p| self.clips[p].status == Status::Synced);
        let nxt = clips[o + 1..].iter().copied().find(|&p| self.clips[p].status == Status::Synced);
        (prev, nxt)
    }

    fn consistent(&self, d: usize, c: usize, t: f64) -> bool {
        let (prev, nxt) = self.neighbors(d, c);
        let cl = &self.clips[c];
        if self.chronology {
            if let Some(p) = prev {
                let p = &self.clips[p];
                if t < p.tt() + p.duration - CHRONO_TOL {
                    return false;
                }
            }
            if let Some(nx) = nxt {
                if t + cl.duration > self.clips[nx].tt() + CHRONO_TOL {
                    return false;
                }
            }
        }
        if self.clock_ok.get(&d).copied().unwrap_or(false) {
            if let Some(cct) = cl.ctime() {
                // device clock must be roughly consistent with the neighbouring anchors
                for nb in [prev, nxt].into_iter().flatten() {
                    let nb = &self.clips[nb];
                    if let Some(nct) = nb.ctime() {
                        let drift = ((t - cct) - (nb.tt() - nct)).abs();
                        if drift > CLOCK_WINDOW.max(0.002 * (cct - nct).abs() + 2.0) {
                            return false;
                        }
                    }
                }
            }
        }
        true
    }

    fn accept(&mut self, c: usize, cand: &Candidate, coarse_sr: usize) {
        let d = cand.target;
        let t = if self.mode.refine { self.refine(c, d, cand.t, coarse_sr) } else { cand.t };
        let dname = self.devices[d].name.clone();
        let cl = &mut self.clips[c];
        cl.t = Some(t);
        cl.status = Status::Synced;
        cl.score = cand.z;
        cl.matched_to = dname.clone();
        let name = cl.name.clone();
        self.done += 1;
        (self.progress)("Sincronizando", self.done as f64 / self.total as f64);
        self.log(&format!("  {name}: sincronizado com {dname} em {t:.3}s (z={:.1})", cand.z));
    }

    fn unsync(&mut self, c: usize) {
        let cl = &mut self.clips[c];
        cl.status = Status::Unsynced;
        cl.t = None;
        cl.score = 0.0;
        cl.matched_to.clear();
        self.done -= 1;
    }

    fn sync_device(&mut self, d: usize, placed: &[usize], second: bool) -> Result<(), Cancelled> {
        let targets: Vec<usize> = placed.iter().copied().filter(|&p| !self.placed(p).is_empty()).collect();
        let mut live: Vec<usize> = self.devices[d].clips.iter().copied()
            .filter(|&c| !matches!(self.clips[c].status, Status::Offline | Status::Synced) && self.audio.contains_key(&c)).collect();
        if !second {
            let names: Vec<&str> = targets.iter().map(|&t| self.devices[t].name.as_str()).collect();
            self.log(&format!("Sincronizando {} ({} clipes) contra {}", self.devices[d].name, self.devices[d].clips.len(), names.join(", ")));
        }
        if targets.is_empty() {
            live.clear();
        }
        if self.mode.audio && !live.is_empty() {
            let sr = self.mode.coarse_sr;
            // pass 1: global search, anchors in order of confidence
            let mut strong: Vec<(usize, Candidate)> = Vec::new();
            for &c in &live {
                self.check()?;
                let mut lst: Vec<Candidate> = Vec::new();
                for &p in &targets {
                    lst.extend(self.global_search(c, p, sr));
                }
                lst.sort_by(|a, b| b.z.total_cmp(&a.z));
                if let Some(best) = lst.first() {
                    if best.z >= Z_STRONG && best.ratio >= R_STRONG {
                        strong.push((c, best.clone()));
                    }
                }
            }
            // anchors may not overlap each other; order/clock are judged afterwards from them
            strong.sort_by(|a, b| b.1.z.total_cmp(&a.1.z));
            for (c, cand) in strong {
                let dur = self.clips[c].duration;
                let ok = self.devices[d].clips.iter().all(|&a| {
                    let a_ = &self.clips[a];
                    a_.status != Status::Synced || a == c || cand.t >= a_.end() - CHRONO_TOL || cand.t + dur <= a_.tt() + CHRONO_TOL
                });
                if ok {
                    self.accept(c, &cand, sr);
                }
            }
            self.settle_order(d);
            self.assess_clock(d);

            // pass 2..: constrained windows (chronology gap, device clock) until nothing changes
            let mut changed = true;
            while changed {
                changed = false;
                let order = self.devices[d].clips.clone();
                for c in order {
                    self.check()?;
                    if matches!(self.clips[c].status, Status::Synced | Status::Offline) || !self.audio.contains_key(&c) {
                        continue;
                    }
                    let mut tries: Vec<((f64, f64), usize)> = Vec::new();
                    let srs: Vec<usize> = std::iter::once(sr).chain(self.mode.extra_srs.iter().copied()).collect();
                    for r in srs {
                        for w in self.windows(d, c, &targets) {
                            tries.push((w, r));
                        }
                    }
                    for ((lo, hi), wsr) in tries {
                        let mut best: Option<Candidate> = None;
                        let dur = self.clips[c].duration;
                        for &p in &targets {
                            let (pt0, pt1) = self.device_span(p);
                            let l2 = lo.max(pt0 - dur + 0.5);
                            let h2 = hi.min(pt1 - 0.5);
                            if h2 <= l2 {
                                continue;
                            }
                            if let Some(cand) = self.local_search(c, p, l2, h2, wsr) {
                                if best.as_ref().is_none_or(|b| cand.z > b.z) {
                                    best = Some(cand);
                                }
                            }
                        }
                        if let Some(b) = best {
                            if b.z >= Z_LOCAL && b.ratio >= R_LOCAL && self.consistent(d, c, b.t) {
                                self.accept(c, &b, wsr);
                                changed = true;
                                break;
                            }
                        }
                    }
                }
            }
        }

        // fallback: creation time with the clock offset of the nearest synced clip,
        // or, when the device clock is not trustworthy, right after the previous synced clip
        let has_anchors = self.devices[d].clips.iter().any(|&c| self.clips[c].status == Status::Synced);
        let order = self.devices[d].clips.clone();
        let first = order.first().copied();
        for c in order {
            if matches!(self.clips[c].status, Status::Synced | Status::Offline) {
                continue;
            }
            let (prev, nxt) = self.neighbors(d, c);
            let nb = prev.or(nxt);
            let cct = self.clips[c].ctime();
            let dur = self.clips[c].duration;
            let by_clock = match (nb, cct) {
                (Some(nbi), Some(cct)) if self.clips[nbi].ctime().is_some() && self.clock_trusted(d, c, prev, nxt) => Some((nbi, cct)),
                _ => None,
            };
            if let Some((nbi, cct)) = by_clock {
                let nbc = &self.clips[nbi];
                let mut t = nbc.tt() + (cct - nbc.ctime().unwrap());
                if self.chronology {
                    if let Some(p) = prev {
                        t = t.max(self.clips[p].end());
                    }
                    if let Some(nx) = nxt {
                        t = t.min(self.clips[nx].tt() - dur);
                    }
                }
                self.clips[c].t = Some(t);
                self.clips[c].status = Status::ByTime;
            } else if nb.is_some() && self.chronology {
                let t = match prev {
                    Some(p) => self.clips[p].end(),
                    None => self.clips[nxt.unwrap()].tt() - dur,
                };
                self.clips[c].t = Some(t);
                self.clips[c].status = Status::ByTime;
            } else if !has_anchors && cct.is_some() && !second {
                // nothing matched at all: assume the device clock agrees with the reference clock
                let rc = targets.iter().flat_map(|&p| self.placed(p)).find(|&r| self.clips[r].ctime().is_some());
                if let Some(rc) = rc {
                    let (rt, rct) = (self.clips[rc].tt(), self.clips[rc].ctime().unwrap());
                    let cct = cct.unwrap();
                    self.clips[c].t = Some(rt + (cct - rct));
                    self.clips[c].status = Status::ByTime;
                    let gap = (cct - rct).abs();
                    if gap > 86400.0 && Some(c) == first {
                        let rdev = self.clips[rc].device.map(|i| self.devices[i].name.clone()).unwrap_or_default();
                        self.log(&format!("[aviso] o relogio de {} difere {:.1} dias do de {}: posicionamento so por horario provavelmente errado",
                                          self.devices[d].name, gap / 86400.0, rdev));
                    }
                }
            }
            if !matches!(self.clips[c].status, Status::Synced | Status::ByTime) {
                self.clips[c].t = Some(self.clips[c].raw_zero);
                self.clips[c].status = Status::Unsynced;
            }
            if !second {
                self.done += 1;
                (self.progress)("Sincronizando", self.done as f64 / self.total as f64);
                self.log(&format!("  {}: SEM correspondencia de audio -> {}", self.clips[c].name, self.clips[c].status.code()));
            }
        }
        Ok(())
    }

    fn inversions(seq: &[f64]) -> usize {
        let mut n = 0;
        for i in 0..seq.len() {
            for j in i + 1..seq.len() {
                if seq[i] > seq[j] {
                    n += 1;
                }
            }
        }
        n
    }

    /// Pick the clip order (when automatic) that agrees best with the audio anchors, then drop
    /// anchors that break chronology (keeping the largest consistent, most confident set).
    fn settle_order(&mut self, d: usize) {
        let anchors: Vec<usize> = self.devices[d].clips.iter().copied().filter(|&c| self.clips[c].status == Status::Synced).collect();
        if self.order == "auto" && anchors.len() >= 3 {
            let mut best: Option<(usize, &str)> = None;
            for k in ["ctime", "name", "xml"] {
                let mut a = anchors.clone();
                let clips = &*self.clips;
                match k {
                    "name" => a.sort_by_key(|&c| clips[c].name.to_lowercase()),
                    "xml" => a.sort_by(|&x, &y| clips[x].raw_zero.total_cmp(&clips[y].raw_zero)),
                    _ => a.sort_by(|&x, &y| clips[x].ctime().unwrap_or(f64::INFINITY).total_cmp(&clips[y].ctime().unwrap_or(f64::INFINITY))
                        .then(clips[x].raw_zero.total_cmp(&clips[y].raw_zero))),
                }
                let ts: Vec<f64> = a.iter().map(|&c| clips[c].tt()).collect();
                let s = Self::inversions(&ts);
                if best.is_none_or(|(bs, _)| s < bs) {
                    best = Some((s, k));
                }
            }
            let (_, k) = best.unwrap();
            if k != "ctime" {
                self.log(&format!("  {}: ordem cronologica por '{}' (horario de criacao nao bate com o audio)", self.devices[d].name, k));
            }
            self.sort_device(d, k);
        }
        let v = self.devices[d].clips.clone();
        for (i, &c) in v.iter().enumerate() {
            self.clips[c].order = i;
        }
        if !self.chronology {
            return;
        }
        // weighted longest increasing subsequence of anchor times in clip order
        let seq: Vec<usize> = v.iter().copied().filter(|&c| self.clips[c].status == Status::Synced).collect();
        let n = seq.len();
        let mut best_w: Vec<f64> = seq.iter().map(|&c| self.clips[c].score).collect();
        let mut back: Vec<i64> = vec![-1; n];
        for i in 0..n {
            for j in 0..i {
                let (sj, si) = (&self.clips[seq[j]], &self.clips[seq[i]]);
                if sj.end() - CHRONO_TOL <= si.tt() && best_w[j] + si.score > best_w[i] {
                    best_w[i] = best_w[j] + si.score;
                    back[i] = j as i64;
                }
            }
        }
        let mut keep = vec![false; n];
        if n > 0 {
            let mut i = 0usize;
            for k in 1..n {
                if best_w[k] > best_w[i] {
                    i = k;
                }
            }
            let mut i = i as i64;
            while i >= 0 {
                keep[i as usize] = true;
                i = back[i as usize];
            }
        }
        for (k, &c) in seq.iter().enumerate() {
            if !keep[k] {
                self.log(&format!("  {}: descartado (quebra a ordem cronologica)", self.clips[c].name));
                self.unsync(c);
            }
        }
    }

    /// Decide from the audio anchors whether this device's creation-time clock is usable, and drop
    /// anchors that contradict a clock that is otherwise consistent (false matches).
    fn assess_clock(&mut self, d: usize) {
        let anchors: Vec<usize> = self.devices[d].clips.iter().copied()
            .filter(|&c| self.clips[c].status == Status::Synced && self.clips[c].ctime().is_some()).collect();
        if anchors.len() < 2 {
            self.clock_ok.insert(d, !anchors.is_empty() && self.mode.clock_window);
            return;
        }
        let offs: Vec<f64> = anchors.iter().map(|&c| self.clips[c].tt() - self.clips[c].ctime().unwrap()).collect();
        let mut sorted = offs.clone();
        sorted.sort_by(f64::total_cmp);
        let m = sorted.len();
        let med = if m % 2 == 1 { sorted[m / 2] } else { (sorted[m / 2 - 1] + sorted[m / 2]) / 2.0 };
        let cts: Vec<f64> = anchors.iter().map(|&c| self.clips[c].ctime().unwrap()).collect();
        let span = cts.iter().cloned().fold(f64::NEG_INFINITY, f64::max) - cts.iter().cloned().fold(f64::INFINITY, f64::min);
        let tol = CLOCK_WINDOW + 0.002 * span;
        let good: Vec<bool> = offs.iter().map(|o| (o - med).abs() <= tol).collect();
        let ok = good.iter().filter(|&&g| g).count() as f64 / good.len() as f64 >= 0.6;
        self.clock_ok.insert(d, ok);
        if ok {
            for (k, &c) in anchors.iter().enumerate() {
                if !good[k] {
                    self.log(&format!("  {}: descartado (contradiz o relogio do dispositivo)", self.clips[c].name));
                    self.unsync(c);
                }
            }
        } else {
            self.log(&format!("  {}: horario de criacao inconsistente com o audio - usando so a cronologia", self.devices[d].name));
        }
    }

    /// Use the device clock when the anchors showed it is consistent, or when both neighbouring
    /// anchors agree on it.
    fn clock_trusted(&self, d: usize, c: usize, prev: Option<usize>, nxt: Option<usize>) -> bool {
        if self.clips[c].ctime().is_none() {
            return false;
        }
        if self.clock_ok.get(&d).copied().unwrap_or(false) {
            return true;
        }
        let (Some(p), Some(n)) = (prev, nxt) else { return false };
        let (p, n) = (&self.clips[p], &self.clips[n]);
        let (Some(pc), Some(nc)) = (p.ctime(), n.ctime()) else { return false };
        ((p.tt() - pc) - (n.tt() - nc)).abs() < 2.0 + 0.002 * (nc - pc).abs()
    }

    /// Search windows for `c`, narrowest first: device-clock prediction, then the chronology gap.
    fn windows(&self, d: usize, c: usize, targets: &[usize]) -> Vec<(f64, f64)> {
        let (prev, nxt) = self.neighbors(d, c);
        let cl = &self.clips[c];
        let mut lo = f64::NEG_INFINITY;
        let mut hi = f64::INFINITY;
        if let Some(p) = prev {
            lo = self.clips[p].tt() + self.clips[p].duration - CHRONO_TOL;
        }
        if let Some(n) = nxt {
            hi = self.clips[n].tt() - cl.duration + CHRONO_TOL;
        }
        let mut out = Vec::new();
        if self.clock_trusted(d, c, prev, nxt) {
            let nb = prev.or(nxt);
            if let Some(nb) = nb.filter(|&nb| self.clips[nb].ctime().is_some()) {
                let nbc = &self.clips[nb];
                let cct = cl.ctime().unwrap();
                let pred = nbc.tt() + (cct - nbc.ctime().unwrap());
                let w = CLOCK_WINDOW + 0.002 * (cct - nbc.ctime().unwrap()).abs();
                let (cl_, ch) = (lo.max(pred - w), hi.min(pred + w));
                if ch > cl_ {
                    out.push((cl_, ch));
                }
                if self.mode.clock_window && self.clock_ok.get(&d).copied().unwrap_or(false) {
                    return out;
                }
            }
        }
        if lo.is_infinite() && hi.is_infinite() {
            return out;
        }
        if lo.is_infinite() || hi.is_infinite() {
            let spans: Vec<(f64, f64)> = targets.iter().map(|&p| self.device_span(p)).collect();
            if lo.is_infinite() {
                lo = spans.iter().map(|s| s.0).fold(f64::INFINITY, f64::min) - cl.duration;
            }
            if hi.is_infinite() {
                hi = spans.iter().map(|s| s.1).fold(f64::NEG_INFINITY, f64::max);
            }
        }
        if hi > lo && hi - lo <= 3600.0 {
            // wider is no constraint at all; pass 1 already covered it
            out.push((lo, hi));
        }
        out
    }
}
