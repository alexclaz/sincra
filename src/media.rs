//! Media probing (duration, creation time, device signature) and cached audio extraction (via ffmpeg).

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime};
use regex::Regex;
use sha1::{Digest, Sha1};

use crate::tr;

/// Analysis sample rate of the cached audio.
pub const AUDIO_SR: usize = 8000;

pub fn command(exe: &Path) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(exe);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

fn exe_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file())
}

/// ffmpeg lookup: $SINCRA_FFMPEG, next to this executable, then PATH (plus Homebrew prefixes on macOS,
/// which are not on PATH for apps started from Finder).
pub fn ffmpeg_exe() -> Result<PathBuf> {
    static CACHE: OnceLock<Option<PathBuf>> = OnceLock::new();
    let found = CACHE.get_or_init(|| {
        if let Some(p) = std::env::var_os("SINCRA_FFMPEG").map(PathBuf::from).filter(|p| p.is_file()) {
            return Some(p);
        }
        let name = exe_name("ffmpeg");
        if let Ok(exe) = std::env::current_exe() {
            let exe = exe.canonicalize().unwrap_or(exe);
            if let Some(dir) = exe.parent() {
                for cand in [dir.join(&name), dir.join("../Resources").join(&name)] {
                    if cand.is_file() {
                        return Some(cand);
                    }
                }
            }
        }
        if let Some(p) = which(&name) {
            return Some(p);
        }
        ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg", "/opt/local/bin/ffmpeg"]
            .iter().map(PathBuf::from).find(|p| p.is_file())
    });
    found.clone().context(tr!("ffmpeg não encontrado. Reinstale o Sincra (o instalador inclui o ffmpeg) ou instale o ffmpeg no PATH", "ffmpeg not found. Reinstall Sincra (the installer includes ffmpeg) or put ffmpeg on the PATH"))
}

pub fn default_cache_dir() -> PathBuf {
    base_cache_dir().join("sincra")
}

pub fn home_dir() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

pub fn base_cache_dir() -> PathBuf {
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| home_dir().join("AppData").join("Local"))
    } else if cfg!(target_os = "macos") {
        home_dir().join("Library").join("Caches")
    } else {
        std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).unwrap_or_else(|| home_dir().join(".cache"))
    }
}

#[derive(Clone, Debug, Default)]
pub struct MediaInfo {
    pub path: String,
    pub exists: bool,
    pub duration: Option<f64>,
    pub has_video: bool,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub audio_streams: u32,
    pub audio_rate: Option<u32>,
    /// Creation (recording start) time, epoch seconds on the device's local clock.
    pub ctime: Option<f64>,
    pub ctime_source: Option<String>,
    pub tags: BTreeMap<String, String>,
}

/// Rough fingerprint of the recording device, used to group clips into cameras/recorders.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Signature {
    Known { has_video: bool, width: Option<u32>, height: Option<u32>, ext: String, stem: String, maker: String },
    Missing,
}

impl MediaInfo {
    pub fn signature(&self) -> Signature {
        let p = Path::new(&self.path);
        let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let stem = re(r"\d+").replace_all(&stem, "#").into_owned();
        let maker = self.tags.get("encoded_by").filter(|s| !s.is_empty())
            .or_else(|| self.tags.get("major_brand").filter(|s| !s.is_empty())).cloned().unwrap_or_default();
        let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy().to_lowercase())).unwrap_or_default();
        Signature::Known { has_video: self.has_video, width: self.width, height: self.height, ext, stem, maker }
    }
}

fn re(pat: &'static str) -> &'static Regex {
    static CACHE: OnceLock<Mutex<HashMap<&'static str, &'static Regex>>> = OnceLock::new();
    let m = CACHE.get_or_init(Default::default);
    let mut g = m.lock().unwrap();
    g.entry(pat).or_insert_with(|| Box::leak(Box::new(Regex::new(pat).unwrap())))
}

// ---------------------------------------------------------------- creation time

pub fn parse_iso(s: &str) -> Option<f64> {
    let s = s.trim().replace('Z', "+00:00");
    let ts = |d: DateTime<chrono::FixedOffset>| d.timestamp() as f64 + d.timestamp_subsec_nanos() as f64 / 1e9;
    for f in ["%Y-%m-%dT%H:%M:%S%.f%:z", "%Y-%m-%d %H:%M:%S%.f%:z", "%Y-%m-%dT%H:%M:%S%.f%z", "%Y-%m-%dT%H:%M%:z"] {
        if let Ok(d) = DateTime::parse_from_str(&s, f) {
            return Some(ts(d));
        }
    }
    for f in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M"] {
        if let Ok(d) = NaiveDateTime::parse_from_str(&s, f) {
            let u = d.and_utc();
            return Some(u.timestamp() as f64 + u.timestamp_subsec_nanos() as f64 / 1e9);
        }
    }
    NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok().map(|d| d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp() as f64)
}

/// Sony XAVC: C0001.MP4 -> C0001M01.XML (NonRealTimeMeta/CreationDate)
fn sony_sidecar_ctime(path: &Path) -> Option<f64> {
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    for suf in ["M01.XML", "M01.xml"] {
        let cand = path.with_file_name(format!("{stem}{suf}"));
        if cand.is_file() {
            let txt = String::from_utf8_lossy(&std::fs::read(&cand).ok()?).into_owned();
            if let Some(m) = re(r#"<CreationDate\s+value="([^"]+)""#).captures(&txt) {
                return parse_iso(&m[1]);
            }
        }
    }
    None
}

/// Broadcast WAV bext chunk: origination date + time_reference (samples since midnight).
fn bwf_ctime(path: &Path) -> Option<f64> {
    let mut f = File::open(path).ok()?;
    let mut head = [0u8; 12];
    f.read_exact(&mut head).ok()?;
    if !(&head[..4] == b"RIFF" || &head[..4] == b"RF64") || &head[8..12] != b"WAVE" {
        return None;
    }
    let mut sr: Option<u32> = None;
    let mut bext: Option<Vec<u8>> = None;
    loop {
        let mut h = [0u8; 8];
        if f.read_exact(&mut h).is_err() {
            break;
        }
        let size = u32::from_le_bytes(h[4..8].try_into().unwrap()) as i64;
        match &h[..4] {
            b"fmt " => {
                let mut b = [0u8; 8];
                f.read_exact(&mut b).ok()?;
                sr = Some(u32::from_le_bytes(b[4..8].try_into().unwrap())).filter(|&v| v > 0);
                f.seek(SeekFrom::Current(size - 8 + (size & 1))).ok()?;
            }
            b"bext" => {
                let mut b = vec![0u8; size as usize];
                let n = f.read(&mut b).ok()?;
                b.truncate(n);
                bext = Some(b);
                f.seek(SeekFrom::Current(size & 1)).ok()?;
            }
            b"data" => break,
            _ => {
                f.seek(SeekFrom::Current(size + (size & 1))).ok()?;
            }
        }
        if sr.is_some() && bext.is_some() {
            break;
        }
    }
    let bext = bext?;
    if bext.len() < 346 {
        return None;
    }
    let ascii = |b: &[u8]| b.iter().filter(|c| c.is_ascii()).map(|&c| c as char).collect::<String>();
    let date = ascii(&bext[320..330]).replace([':', '/'], "-");
    let tref = u64::from_le_bytes(bext[338..346].try_into().unwrap());
    let timestr = ascii(&bext[330..338]).replace(['-', '.'], ":");
    let day = NaiveDate::parse_from_str(&date, "%Y-%m-%d").ok()?; // device local clock, treated as naive
    let day_ts = day.and_hms_opt(0, 0, 0)?.and_utc().timestamp() as f64;
    if let (Some(sr), true) = (sr, tref != 0) {
        return Some(day_ts + tref as f64 / sr as f64);
    }
    let t = NaiveTime::parse_from_str(&timestr, "%H:%M:%S").ok()?;
    use chrono::Timelike;
    Some(day_ts + (t.hour() * 3600 + t.minute() * 60 + t.second()) as f64)
}

pub fn probe(path: &str) -> MediaInfo {
    let p = Path::new(path);
    let mut info = MediaInfo { path: path.to_string(), exists: true, ..Default::default() };
    if !p.exists() {
        info.exists = false;
        return info;
    }
    let txt = ffmpeg_exe().ok()
        .and_then(|exe| command(&exe).args(["-hide_banner", "-i"]).arg(p).stdin(Stdio::null()).output().ok())
        .map(|o| String::from_utf8_lossy(&o.stderr).into_owned()).unwrap_or_default();
    if let Some(m) = re(r"Duration:\s*(\d+):(\d+):([\d.]+)").captures(&txt) {
        let h: f64 = m[1].parse().unwrap_or(0.0);
        let mi: f64 = m[2].parse().unwrap_or(0.0);
        let s: f64 = m[3].parse().unwrap_or(0.0);
        info.duration = Some(h * 3600.0 + mi * 60.0 + s);
    }
    let tag_re = re(r"^\s{4}(\w+)\s*:\s*(.*)$");
    let mut in_global = false;
    for line in txt.lines() {
        if line.starts_with("Input #0") {
            in_global = true;
            continue;
        }
        if line.trim().starts_with("Duration:") || line.contains("Stream #") {
            in_global = false;
        }
        if in_global {
            if let Some(tm) = tag_re.captures(line) {
                info.tags.entry(tm[1].to_lowercase()).or_insert_with(|| tm[2].trim().to_string());
            }
        }
        if line.contains("Stream #") && line.contains(": Video:") && !line.contains("attached pic") {
            info.has_video = true;
            if let Some(wm) = re(r"(\d{2,5})x(\d{2,5})").captures(line) {
                info.width = wm[1].parse().ok();
                info.height = wm[2].parse().ok();
            }
        }
        if line.contains("Stream #") && line.contains(": Audio:") {
            info.audio_streams += 1;
            if info.audio_rate.is_none() {
                if let Some(am) = re(r"(\d+) Hz").captures(line) {
                    info.audio_rate = am[1].parse().ok();
                }
            }
        }
    }
    let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let found = sony_sidecar_ctime(p).map(|t| (t, "sony-xml"))
        .or_else(|| if ext == "wav" || ext == "bwf" { bwf_ctime(p).map(|t| (t, "bwf")) } else { None })
        .or_else(|| info.tags.get("creation_time").filter(|s| s.contains('T')).and_then(|s| parse_iso(s)).map(|t| (t, "container")));
    if let Some((t, src)) = found {
        info.ctime = Some(t);
        info.ctime_source = Some(src.into());
    } else {
        let mtime = std::fs::metadata(p).and_then(|m| m.modified()).ok()
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs_f64()).unwrap_or(0.0);
        info.ctime = Some(mtime - info.duration.unwrap_or(0.0));
        info.ctime_source = Some("mtime".into());
    }
    info
}

// ---------------------------------------------------------------- audio

pub type Audio = Arc<Vec<f32>>;

/// Decoded mono 8 kHz audio, cached on disk as .npy (same files as the Python version).
pub struct AudioCache {
    pub dir: PathBuf,
    pub enabled: bool,
    mem: Mutex<HashMap<String, Audio>>,
}

impl AudioCache {
    pub fn new(cache_dir: Option<&str>, enabled: bool) -> Self {
        let dir = cache_dir.filter(|s| !s.is_empty()).map(PathBuf::from).unwrap_or_else(default_cache_dir);
        if enabled {
            let _ = std::fs::create_dir_all(&dir);
        }
        AudioCache { dir, enabled, mem: Mutex::new(HashMap::new()) }
    }

    fn key(path: &str) -> Result<String> {
        let md = std::fs::metadata(path)?;
        let mtime_ns = md.modified()?.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let abs = std::path::absolute(path).unwrap_or_else(|_| PathBuf::from(path));
        let s = format!("{}|{}|{}|{}", abs.display(), md.len(), mtime_ns, AUDIO_SR);
        let h = Sha1::digest(s.as_bytes());
        let hex: String = h.iter().map(|b| format!("{b:02x}")).collect();
        let name = Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Ok(format!("{name}.{}.npy", &hex[..16]))
    }

    pub fn load(&self, path: &str) -> Result<Audio> {
        if let Some(a) = self.mem.lock().unwrap().get(path) {
            return Ok(a.clone());
        }
        let cf = if self.enabled { Some(self.dir.join(Self::key(path)?)) } else { None };
        let cached = cf.as_ref().filter(|p| p.is_file()).and_then(|p| read_npy_f32(p).ok());
        let a = match cached {
            Some(a) => a,
            None => {
                let a = Self::decode(path)?;
                if let Some(cf) = &cf {
                    let tmp = cf.with_extension("tmp.npy");
                    if write_npy_f32(&tmp, &a).is_ok() {
                        let _ = std::fs::rename(&tmp, cf);
                    }
                }
                a
            }
        };
        let a = Arc::new(a);
        self.mem.lock().unwrap().insert(path.to_string(), a.clone());
        Ok(a)
    }

    /// Audio already in the disk cache (no decoding), for waveform previews.
    pub fn load_cached(&self, path: &str) -> Option<Audio> {
        if !self.enabled {
            return None;
        }
        let cf = self.dir.join(Self::key(path).ok()?);
        read_npy_f32(&cf).ok().map(Arc::new)
    }

    fn decode(path: &str) -> Result<Vec<f32>> {
        let exe = ffmpeg_exe()?;
        let o = command(&exe)
            .args(["-v", "error", "-i"]).arg(path)
            .args(["-vn", "-ac", "1", "-ar", &AUDIO_SR.to_string(), "-f", "f32le", "-"])
            .stdin(Stdio::null()).output().context(tr!("falha ao executar o ffmpeg", "failed to run ffmpeg"))?;
        if !o.status.success() && o.stdout.is_empty() {
            let err = String::from_utf8_lossy(&o.stderr);
            let tail: String = err.chars().rev().take(300).collect::<Vec<_>>().into_iter().rev().collect();
            bail!(tr!("Erro ao decodificar o áudio de {}: {}", "Could not decode the audio of {}: {}", path, tail));
        }
        Ok(o.stdout.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect())
    }

    pub fn clear(&self) {
        self.mem.lock().unwrap().clear();
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                if e.path().extension().is_some_and(|x| x == "npy") {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
}

fn read_npy_f32(path: &Path) -> Result<Vec<f32>> {
    let bytes = std::fs::read(path)?;
    if bytes.len() < 10 || &bytes[..6] != b"\x93NUMPY" {
        bail!("npy invalido");
    }
    let (hlen, off) = match bytes[6] {
        1 => (u16::from_le_bytes([bytes[8], bytes[9]]) as usize, 10),
        _ => (u32::from_le_bytes(bytes[8..12].try_into()?) as usize, 12),
    };
    let header = std::str::from_utf8(&bytes[off..off + hlen])?;
    if !header.contains("'<f4'") || header.contains("'fortran_order': True") {
        bail!("npy com tipo inesperado");
    }
    let data = &bytes[off + hlen..];
    Ok(data.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect())
}

fn write_npy_f32(path: &Path, a: &[f32]) -> Result<()> {
    let mut header = format!("{{'descr': '<f4', 'fortran_order': False, 'shape': ({},), }}", a.len());
    let total = 10 + header.len() + 1;
    let pad = (64 - total % 64) % 64;
    header.push_str(&" ".repeat(pad));
    header.push('\n');
    let mut f = std::io::BufWriter::new(File::create(path)?);
    f.write_all(b"\x93NUMPY\x01\x00")?;
    f.write_all(&(header.len() as u16).to_le_bytes())?;
    f.write_all(header.as_bytes())?;
    let mut buf = Vec::with_capacity(a.len() * 4);
    for v in a {
        buf.extend_from_slice(&v.to_le_bytes());
    }
    f.write_all(&buf)?;
    f.flush()?;
    Ok(())
}
