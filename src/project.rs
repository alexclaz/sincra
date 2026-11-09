//! Glue: XML sequence -> devices/clips -> engine -> synced XML.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::Result;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::engine::{pyround, Cancelled, Clip, Device, LogFn, ProgressFn, Status, SyncEngine, DEFAULT_MODE};
use crate::media::{probe, AudioCache, MediaInfo, Signature};
use crate::xmeml::{write_synced, Kind, Sequence, XmemlDocument};

pub const LABEL_COLORS: &[&str] = &["Violet", "Iris", "Caribbean", "Lavender", "Cerulean", "Forest", "Rose", "Mango",
                                    "Purple", "Blue", "Teal", "Magenta", "Tan", "Green", "Brown", "Yellow"];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Options {
    pub mode: String,
    /// auto | ctime | xml | name
    pub order: String,
    /// keep the chronology of each device's clips
    pub chronology: bool,
    /// device index, None = automatic
    pub reference: Option<usize>,
    /// Premiere label color for synced clips
    pub label_synced: Option<String>,
    pub label_unsynced: Option<String>,
    pub suffix: String,
    pub use_cache: bool,
    pub cache_dir: Option<String>,
    /// (old prefix, new prefix)
    pub path_map: Vec<(String, String)>,
    /// where to look for missing files
    pub search_dirs: Vec<String>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            mode: DEFAULT_MODE.into(),
            order: "auto".into(),
            chronology: true,
            reference: None,
            label_synced: None,
            label_unsynced: Some("Rose".into()),
            suffix: " - synced".into(),
            use_cache: true,
            cache_dir: None,
            path_map: Vec::new(),
            search_dirs: Vec::new(),
        }
    }
}

pub fn remap_path(path: &str, opts: &Options, index: Option<&HashMap<String, String>>) -> String {
    let norm = path.replace('\\', "/");
    for (old, new) in &opts.path_map {
        let o = old.replace('\\', "/").trim_end_matches('/').to_string();
        let nl = norm.to_lowercase();
        let ol = o.to_lowercase();
        if nl.starts_with(&(ol.clone() + "/")) || nl == ol {
            let cand = new.replace('\\', "/").trim_end_matches('/').to_string() + &norm[o.len()..];
            if Path::new(&cand).exists() {
                return cand;
            }
        }
    }
    if Path::new(path).exists() {
        return path.to_string();
    }
    if let Some(idx) = index {
        let name = Path::new(&norm).file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        if let Some(hit) = idx.get(&name) {
            return hit.clone();
        }
    }
    path.to_string()
}

fn index_dirs(dirs: &[String]) -> HashMap<String, String> {
    let mut idx = HashMap::new();
    for d in dirs {
        for e in walkdir::WalkDir::new(d).into_iter().flatten() {
            if e.file_type().is_file() {
                idx.entry(e.file_name().to_string_lossy().to_lowercase())
                    .or_insert_with(|| e.path().to_string_lossy().into_owned());
            }
        }
    }
    idx
}

pub struct Project {
    pub xml_path: String,
    pub opts: Options,
    pub log: LogFn,
    pub doc: XmemlDocument,
    pub seq_index: usize,
    pub seq: Sequence,
    pub devices: Vec<Device>,
    pub clips: Vec<Clip>,
    pub synced: bool,
}

pub struct Row {
    pub device: String,
    pub clip: String,
    pub status: Status,
    pub t: Option<f64>,
    pub score: f64,
    pub matched: String,
    pub ctime_source: Option<String>,
}

impl Project {
    pub fn new(xml_path: &str, sequence: usize, opts: Options, log: LogFn) -> Result<Self> {
        let doc = XmemlDocument::open(xml_path)?;
        let seq = doc.sequence(sequence)?;
        Ok(Project { xml_path: xml_path.into(), opts, log, doc, seq_index: sequence, seq, devices: vec![], clips: vec![], synced: false })
    }

    // ------------------------------------------------------------- loading
    pub fn load(&mut self, progress: Option<ProgressFn>) -> Result<()> {
        let progress = progress.unwrap_or_else(|| Arc::new(|_: &str, _: f64| {}));
        crate::media::ffmpeg_exe()?;
        let idx = if self.opts.search_dirs.is_empty() { None } else { Some(index_dirs(&self.opts.search_dirs)) };
        let insts = self.seq.instances.clone();
        let paths: HashMap<String, String> = insts.iter().map(|i| (i.path.clone(), remap_path(&i.path, &self.opts, idx.as_ref()))).collect();
        let mut uniq: Vec<String> = paths.values().cloned().collect::<HashSet<_>>().into_iter().collect();
        uniq.sort();
        let done = AtomicUsize::new(0);
        let total = uniq.len().max(1);
        let pool = rayon::ThreadPoolBuilder::new().num_threads(8).build()?;
        let infos: HashMap<String, MediaInfo> = pool.install(|| {
            uniq.par_iter().map(|p| {
                let info = probe(p);
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                progress("Analisando midia", n as f64 / total as f64);
                (p.clone(), info)
            }).collect()
        });
        let fps = self.seq.fps;
        let mut group_order: Vec<((Kind, usize), Signature)> = Vec::new();
        let mut groups: HashMap<((Kind, usize), Signature), Vec<usize>> = HashMap::new();
        self.clips.clear();
        self.devices.clear();
        for inst in &insts {
            let real = &paths[&inst.path];
            let info = infos[real].clone();
            if !info.exists {
                (self.log)(&format!("[aviso] arquivo nao encontrado: {}", inst.path));
            }
            let sig = if info.exists { info.signature() } else { Signature::Missing };
            let exists = info.exists;
            let clip = Clip {
                key: inst.key.clone(), name: inst.name.clone(), path: real.clone(), info,
                raw_zero: self.seq.zero_frame(inst) / fps, duration: 0.0, device: None, order: 0, t: None,
                status: if exists { Status::Unsynced } else { Status::Offline }, score: 0.0, matched_to: String::new(),
            };
            self.clips.push(clip);
            let key = (self.seq.primary_track(inst), sig);
            groups.entry(key.clone()).or_insert_with(|| {
                group_order.push(key.clone());
                Vec::new()
            }).push(self.clips.len() - 1);
        }
        // devices sorted: cameras (by track) then recorders
        group_order.sort_by_key(|((kind, track), _)| (*kind != Kind::Video, *track));
        let (mut cams, mut recs) = (0, 0);
        for (n, key) in group_order.iter().enumerate() {
            let clips = groups.remove(key).unwrap();
            let is_cam = clips.iter().any(|&c| self.clips[c].info.has_video);
            let name = if is_cam {
                cams += 1;
                format!("Camera {cams}")
            } else {
                recs += 1;
                format!("Gravador {recs}")
            };
            for &c in &clips {
                self.clips[c].device = Some(n);
            }
            self.devices.push(Device { idx: n, name, kind: if is_cam { "camera".into() } else { "recorder".into() }, clips });
        }
        (self.log)(&format!("Sequencia '{}': {} clipes, {} camera(s), {} gravador(es)", self.seq.name, self.clips.len(), cams, recs));
        Ok(())
    }

    // ------------------------------------------------------------- sync
    pub fn reset_sync(&mut self) {
        for c in &mut self.clips {
            if c.status != Status::Offline {
                c.t = None;
                c.status = Status::Unsynced;
                c.score = 0.0;
                c.matched_to.clear();
            }
        }
        self.synced = false;
    }

    pub fn synchronize(&mut self, progress: Option<ProgressFn>, cancelled: Option<Arc<AtomicBool>>) -> Result<(), Cancelled> {
        let cache = AudioCache::new(self.opts.cache_dir.as_deref(), self.opts.use_cache);
        let loader = |p: &str| cache.load(p);
        let progress = progress.unwrap_or_else(|| Arc::new(|_: &str, _: f64| {}));
        let cancelled = cancelled.unwrap_or_default();
        let mut eng = SyncEngine::new(&mut self.clips, &mut self.devices, &loader, &self.opts.mode, &self.opts.order,
                                      self.opts.reference, self.opts.chronology, self.log.clone(), progress, cancelled);
        eng.run()?;
        self.synced = true;
        Ok(())
    }

    pub fn placements(&mut self) -> (HashMap<String, (i64, Option<String>)>, HashMap<String, usize>) {
        let fps = self.seq.fps;
        for c in &mut self.clips {
            if c.t.is_none() {
                c.t = Some(c.raw_zero);
            }
        }
        let by_key: HashMap<String, usize> = self.clips.iter().enumerate().map(|(i, c)| (c.key.clone(), i)).collect();
        let anchor_in: HashMap<String, i64> = self.seq.instances.iter().map(|i| (i.key.clone(), self.seq.anchor(i).in_)).collect();
        let devices_by_name: HashMap<String, usize> = self.devices.iter().enumerate().map(|(i, d)| (d.name.clone(), i)).collect();
        // Clips land on whole frames, so a clip synced to another one is positioned relative to the
        // *rounded* frame of its partner (otherwise sub-frame rounding of long recorder files leaks in).
        let mut zero: HashMap<usize, f64> = HashMap::new();
        let mut new_start: HashMap<usize, i64> = HashMap::new();
        let mut visiting: HashSet<usize> = HashSet::new();

        #[allow(clippy::too_many_arguments)]
        fn place(c: usize, clips: &[Clip], devices: &[Device], devices_by_name: &HashMap<String, usize>, anchor_in: &HashMap<String, i64>,
                 fps: f64, zero: &mut HashMap<usize, f64>, new_start: &mut HashMap<usize, i64>, visiting: &mut HashSet<usize>) {
            if new_start.contains_key(&c) || !visiting.insert(c) {
                return;
            }
            let cl = &clips[c];
            let ct = cl.t.unwrap_or(0.0);
            let mut exact = ct * fps;
            if cl.status == Status::Synced {
                if let Some(&di) = devices_by_name.get(&cl.matched_to) {
                    let mut best: Option<(usize, f64)> = None;
                    for &p in &devices[di].clips {
                        let pc = &clips[p];
                        if pc.status.placed() && pc.t.unwrap_or(0.0) < cl.end() && pc.end() > ct {
                            let ov = pc.end().min(cl.end()) - pc.t.unwrap_or(0.0).max(ct);
                            if best.is_none_or(|(_, b)| ov > b) {
                                best = Some((p, ov));
                            }
                        }
                    }
                    if let Some((p, _)) = best {
                        place(p, clips, devices, devices_by_name, anchor_in, fps, zero, new_start, visiting);
                        if let Some(&zp) = zero.get(&p) {
                            exact = zp + (ct - clips[p].t.unwrap_or(0.0)) * fps;
                        }
                    }
                }
            }
            let ain = anchor_in[&cl.key];
            let ns = pyround(exact + ain as f64) as i64;
            new_start.insert(c, ns);
            zero.insert(c, (ns - ain) as f64);
        }

        for d in &self.devices {
            for &c in &d.clips {
                place(c, &self.clips, &self.devices, &devices_by_name, &anchor_in, fps, &mut zero, &mut new_start, &mut visiting);
            }
        }
        let shift = -new_start.values().copied().min().unwrap_or(0);
        let mut out = HashMap::new();
        let mut track_of = HashMap::new();
        for inst in &self.seq.instances {
            let ci = by_key[&inst.key];
            let c = &self.clips[ci];
            let color = if c.status.placed() { self.opts.label_synced.clone() } else { self.opts.label_unsynced.clone() };
            let color = color.filter(|s| !s.is_empty());
            let ns = new_start.get(&ci).copied().unwrap_or(self.seq.anchor(inst).start);
            out.insert(inst.key.clone(), (ns + shift - self.seq.anchor(inst).start, color));
            track_of.insert(inst.key.clone(), c.device.unwrap_or(0));
        }
        (out, track_of)
    }

    pub fn default_output(&self) -> PathBuf {
        let p = Path::new(&self.xml_path);
        let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
        p.with_file_name(format!("{stem}{}{ext}", self.opts.suffix))
    }

    pub fn export(&mut self, out_path: Option<&Path>) -> Result<PathBuf> {
        let out_path = out_path.map(Path::to_path_buf).unwrap_or_else(|| self.default_output());
        let (placements, track_of) = self.placements();
        // the writer mutates the tree, so always write from a freshly parsed copy
        let mut fresh = XmemlDocument::open(&self.xml_path)?;
        let seq = fresh.sequence(self.seq_index)?;
        write_synced(&mut fresh, &seq, &placements, &track_of, &out_path, &self.opts.suffix)?;
        (self.log)(&format!("XML exportado: {}", out_path.display()));
        Ok(out_path)
    }

    pub fn summary(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for d in &self.devices {
            for &c in &d.clips {
                let c = &self.clips[c];
                rows.push(Row { device: d.name.clone(), clip: c.name.clone(), status: c.status, t: c.t, score: c.score,
                                matched: c.matched_to.clone(), ctime_source: c.info.ctime_source.clone() });
            }
        }
        rows
    }
}
