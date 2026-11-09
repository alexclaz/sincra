//! Final Cut Pro 7 XML (xmeml) reading and synced-timeline writing.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::dom::{self, Element, Node};
use crate::tr;

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let h = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(v) = h {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn pathurl_to_path(url: &str) -> String {
    let url = url.trim();
    if url.len() < 5 || !url[..5].eq_ignore_ascii_case("file:") {
        return percent_decode(url);
    }
    let rest = &url[5..];
    let (netloc, path) = if let Some(r) = rest.strip_prefix("//") {
        match r.find('/') {
            Some(i) => (&r[..i], &r[i..]),
            None => (r, ""),
        }
    } else {
        ("", rest)
    };
    let path = percent_decode(path);
    let pb = path.as_bytes();
    if pb.len() >= 3 && pb[0] == b'/' && pb[1].is_ascii_alphabetic() && pb[2] == b':' {
        return path[1..].to_string();
    }
    if !netloc.is_empty() && !netloc.eq_ignore_ascii_case("localhost") {
        return format!("//{netloc}{path}");
    }
    path
}

pub type Rate = (i64, bool);

fn rate_of(el: Option<&Element>, default: Rate) -> Rate {
    let Some(el) = el else { return default };
    let tb = el.findtext_nonempty("rate/timebase");
    let ntsc = el.findtext("rate/ntsc").unwrap_or_else(|| "FALSE".into()).trim().eq_ignore_ascii_case("TRUE");
    match tb.and_then(|t| t.trim().parse::<f64>().ok()) {
        Some(t) => (t as i64, ntsc),
        None => default,
    }
}

pub fn fps_of(rate: Rate) -> f64 {
    let (tb, ntsc) = rate;
    if ntsc {
        tb as f64 * 1000.0 / 1001.0
    } else {
        tb as f64
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Video,
    Audio,
}

impl Kind {
    pub fn tag(self) -> &'static str {
        match self {
            Kind::Video => "video",
            Kind::Audio => "audio",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ClipItem {
    pub kind: Kind,
    pub track_no: usize,  // 1-based within kind
    pub child_idx: usize, // index of the clipitem node inside its <track>
    pub id: String,
    pub file_id: Option<String>,
    pub start: i64,
    pub end: i64,
    pub in_: i64,
    pub out: i64,
    pub rate: Rate,
    pub name: String,
}

/// One placement of a media file on the timeline (its linked video + audio clipitems).
#[derive(Clone, Debug)]
pub struct Instance {
    pub key: String,
    pub file_id: String,
    pub path: String,
    pub name: String,
    pub items: Vec<usize>, // indexes into Sequence::items, anchor first
}

#[derive(Clone, Debug)]
pub struct Sequence {
    pub path: Vec<usize>, // element path from the document root
    pub name: String,
    pub rate: Rate,
    pub fps: f64,
    pub items: Vec<ClipItem>,
    pub instances: Vec<Instance>,
}

impl Sequence {
    pub fn anchor(&self, inst: &Instance) -> &ClipItem {
        &self.items[inst.items[0]]
    }

    pub fn has_video_item(&self, inst: &Instance) -> bool {
        inst.items.iter().any(|&i| self.items[i].kind == Kind::Video)
    }

    pub fn primary_track(&self, inst: &Instance) -> (Kind, usize) {
        let v = inst.items.iter().filter(|&&i| self.items[i].kind == Kind::Video).map(|&i| self.items[i].track_no).min();
        match v {
            Some(t) => (Kind::Video, t),
            None => (Kind::Audio, inst.items.iter().map(|&i| self.items[i].track_no).min().unwrap_or(1)),
        }
    }

    /// Timeline frame where the source media's time zero falls (raw timeline).
    pub fn zero_frame(&self, inst: &Instance) -> f64 {
        let a = self.anchor(inst);
        (a.start - a.in_) as f64
    }

    fn parse(root: &Element, path: Vec<usize>) -> Sequence {
        let el = root.at_path(&path).expect("sequence path");
        let rate = rate_of(Some(el), (24, false));
        let mut seq = Sequence {
            name: el.findtext_nonempty("name").unwrap_or_else(|| "Sequence".into()),
            rate,
            fps: fps_of(rate),
            items: Vec::new(),
            instances: Vec::new(),
            path,
        };
        let mut files: HashMap<String, &Element> = HashMap::new();
        let mut all_files = Vec::new();
        el.iter_named("file", &mut all_files);
        for f in all_files {
            if let Some(fid) = f.attr("id").filter(|s| !s.is_empty()) {
                if f.child("pathurl").is_some() || !files.contains_key(fid) {
                    files.insert(fid.to_string(), f);
                }
            }
        }
        for kind in [Kind::Video, Kind::Audio] {
            let Some(media) = el.find(&format!("media/{}", kind.tag())) else { continue };
            for (ti, tr) in media.children_named("track").enumerate() {
                for (ci_idx, node) in tr.children.iter().enumerate() {
                    let Node::Element(ci) = node else { continue };
                    if ci.name != "clipitem" {
                        continue;
                    }
                    let num = |t: &str| ci.findtext(t).and_then(|s| s.trim().parse::<i64>().ok());
                    let (Some(start), Some(end), Some(in_), Some(out)) = (num("start"), num("end"), num("in"), num("out")) else {
                        continue;
                    };
                    if start < 0 || end < 0 {
                        continue; // transition-adjacent placeholders
                    }
                    seq.items.push(ClipItem {
                        kind,
                        track_no: ti + 1,
                        child_idx: ci_idx,
                        id: ci.attr("id").unwrap_or("").to_string(),
                        file_id: ci.child("file").and_then(|f| f.attr("id")).map(str::to_string),
                        start,
                        end,
                        in_,
                        out,
                        rate: rate_of(Some(ci), rate),
                        name: ci.findtext("name").unwrap_or_default(),
                    });
                }
            }
        }
        // Clipitems of the same file whose media time zero lands on the same timeline frame are one
        // placement (linked video + audio channels); they must move together.
        let mut order: Vec<(String, i64)> = Vec::new();
        let mut groups: HashMap<(String, i64), Vec<usize>> = HashMap::new();
        for (i, it) in seq.items.iter().enumerate() {
            let Some(fid) = &it.file_id else { continue };
            if !files.contains_key(fid) {
                continue;
            }
            let k = (fid.clone(), it.start - it.in_);
            groups.entry(k.clone()).or_insert_with(|| {
                order.push(k.clone());
                Vec::new()
            }).push(i);
        }
        let mut glist: Vec<Vec<usize>> = order.iter().map(|k| groups.remove(k).unwrap()).collect();
        glist.sort_by_key(|g| (seq.items[g[0]].start, seq.items[g[0]].track_no));
        for (n, mut items) in glist.into_iter().enumerate() {
            items.sort_by_key(|&i| (seq.items[i].kind != Kind::Video, seq.items[i].track_no));
            let fid = seq.items[items[0]].file_id.clone().unwrap();
            let fel = files[&fid];
            let url = fel.findtext("pathurl").unwrap_or_default();
            let name = fel.findtext_nonempty("name")
                .or_else(|| Some(seq.items[items[0]].name.clone()).filter(|s| !s.is_empty()))
                .unwrap_or_else(|| url.rsplit('/').next().unwrap_or("").to_string());
            seq.instances.push(Instance { key: format!("i{n}"), file_id: fid, path: pathurl_to_path(&url), name, items });
        }
        seq
    }
}

pub struct XmemlDocument {
    pub path: String,
    pub root: Element,
    sequence_paths: Vec<Vec<usize>>,
}

impl XmemlDocument {
    pub fn open(path: &str) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| tr!("Não foi possível ler {}", "Could not read {}", path))?;
        let root = dom::parse(&bytes)?;
        if root.name != "xmeml" {
            bail!(tr!("Isto não é um XML Final Cut Pro 7 (xmeml)", "This is not a Final Cut Pro 7 XML (xmeml)"));
        }
        let sequence_paths = root.iter_paths("sequence");
        if sequence_paths.is_empty() {
            bail!(tr!("Não há sequências no arquivo XML", "The XML file has no sequences"));
        }
        Ok(XmemlDocument { path: path.to_string(), root, sequence_paths })
    }

    pub fn sequence_names(&self) -> Vec<String> {
        self.sequence_paths.iter().enumerate()
            .map(|(i, p)| self.root.at_path(p).and_then(|s| s.findtext_nonempty("name")).unwrap_or_else(|| format!("Sequence {}", i + 1)))
            .collect()
    }

    pub fn sequence_count(&self) -> usize {
        self.sequence_paths.len()
    }

    pub fn sequence(&self, index: usize) -> Result<Sequence> {
        let p = self.sequence_paths.get(index).with_context(|| tr!("A sequência {} não existe", "Sequence {} does not exist", index))?;
        Ok(Sequence::parse(&self.root, p.clone()))
    }
}

// ------------------------------------------------------------------------ writer

fn set_label(el: &mut Element, color: Option<&str>) {
    el.remove_child("labels");
    if let Some(color) = color.filter(|c| !c.is_empty()) {
        let mut labels = Element::new("labels");
        let mut l2 = Element::new("label2");
        l2.set_text(color);
        labels.children.push(Node::Element(l2));
        let idx = el.children.iter().position(|n| matches!(n, Node::Element(e) if e.name == "file")).unwrap_or(el.children.len());
        el.children.insert(idx, Node::Element(labels));
    }
}

fn track_el_mut(seq_el: &mut Element, kind: Kind, track_no: usize) -> Option<&mut Element> {
    let media = seq_el.find_mut(&format!("media/{}", kind.tag()))?;
    media.children.iter_mut().filter_map(|n| match n {
        Node::Element(e) if e.name == "track" => Some(e),
        _ => None,
    }).nth(track_no - 1)
}

/// placements: instance key -> (frame delta, label color). track_of: instance key -> device index
/// (every device gets its own tracks). Mutates `doc` and writes it to `out_path`.
pub fn write_synced(doc: &mut XmemlDocument, seq: &Sequence, placements: &HashMap<String, (i64, Option<String>)>,
                    track_of: &HashMap<String, usize>, out_path: &Path, name_suffix: &str) -> Result<()> {
    let mut items = seq.items.clone();
    let mut inst_of_item: HashMap<usize, usize> = HashMap::new();
    for (ii, inst) in seq.instances.iter().enumerate() {
        for &it in &inst.items {
            inst_of_item.insert(it, ii);
        }
    }
    let seq_el = doc.root.at_path_mut(&seq.path).context("sequencia")?;

    // 1. retime clipitems
    for inst in &seq.instances {
        let (delta, color) = placements.get(&inst.key).cloned().unwrap_or((0, None));
        for &i in &inst.items {
            let it = &mut items[i];
            it.start += delta;
            it.end += delta;
            let (start, end, kind, track_no, child_idx) = (it.start, it.end, it.kind, it.track_no, it.child_idx);
            if let Some(Node::Element(ci)) = track_el_mut(seq_el, kind, track_no).and_then(|t| t.children.get_mut(child_idx)) {
                ci.set_child_text("start", &start.to_string());
                ci.set_child_text("end", &end.to_string());
                set_label(ci, color.as_deref());
            }
        }
    }

    // 2. rebuild tracks: per original track, split by device, then by overlap
    for kind in [Kind::Video, Kind::Audio] {
        let Some(media) = seq_el.find_mut(&format!("media/{}", kind.tag())) else { continue };
        let track_pos: Vec<usize> = media.children.iter().enumerate()
            .filter(|(_, n)| matches!(n, Node::Element(e) if e.name == "track")).map(|(i, _)| i).collect();
        let insert_at = track_pos.first().copied().unwrap_or(media.children.len());
        let mut old_tracks: Vec<Element> = Vec::new();
        for &p in track_pos.iter().rev() {
            if let Node::Element(e) = media.children.remove(p) {
                old_tracks.push(e);
            }
        }
        old_tracks.reverse();
        let mut new_tracks: Vec<Element> = Vec::new();
        for (ti, tr) in old_tracks.into_iter().enumerate() {
            let track_items: Vec<usize> = (0..items.len())
                .filter(|&i| items[i].kind == kind && items[i].track_no == ti + 1 && inst_of_item.contains_key(&i)).collect();
            let mut buckets: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
            for &i in &track_items {
                let key = &seq.instances[inst_of_item[&i]].key;
                buckets.entry(track_of.get(key).copied().unwrap_or(0)).or_default().push(i);
            }
            if buckets.is_empty() {
                new_tracks.push(tr);
                continue;
            }
            let others: Vec<Element> = tr.elements()
                .filter(|c| !matches!(c.name.as_str(), "clipitem" | "transitionitem" | "generatoritem")).cloned().collect();
            let attrs = tr.attrs.clone();
            let mut slots: Vec<Option<Node>> = tr.children.into_iter().map(Some).collect();
            for (_, bitems) in buckets {
                let mut sorted = bitems.clone();
                sorted.sort_by_key(|&i| items[i].start);
                let mut lanes: Vec<Vec<usize>> = Vec::new();
                for i in sorted {
                    match lanes.iter_mut().find(|l| items[*l.last().unwrap()].end <= items[i].start) {
                        Some(l) => l.push(i),
                        None => lanes.push(vec![i]),
                    }
                }
                for lane in lanes {
                    let mut nt = Element { name: "track".into(), attrs: attrs.clone(), children: Vec::new() };
                    for i in lane {
                        if let Some(n) = slots[items[i].child_idx].take() {
                            nt.children.push(n);
                        }
                    }
                    for c in &others {
                        nt.children.push(Node::Element(c.clone()));
                    }
                    new_tracks.push(nt);
                }
            }
            let gens: Vec<Node> = slots.into_iter().flatten()
                .filter(|n| matches!(n, Node::Element(e) if e.name == "generatoritem")).collect();
            if !gens.is_empty() {
                new_tracks.push(Element { name: "track".into(), attrs: attrs.clone(), children: gens });
            }
        }
        for (k, nt) in new_tracks.into_iter().enumerate() {
            media.children.insert(insert_at + k, Node::Element(nt));
        }
    }

    // 3. refresh Premiere-style link indexes (trackindex/clipindex)
    let mut pos: HashMap<String, (usize, usize)> = HashMap::new();
    for kind in [Kind::Video, Kind::Audio] {
        let Some(media) = seq_el.find(&format!("media/{}", kind.tag())) else { continue };
        for (ti, tr) in media.children_named("track").enumerate() {
            for (cn, ci) in tr.children_named("clipitem").enumerate() {
                if let Some(id) = ci.attr("id").filter(|s| !s.is_empty()) {
                    pos.insert(id.to_string(), (ti + 1, cn + 1));
                }
            }
        }
    }
    for kind in [Kind::Video, Kind::Audio] {
        let Some(media) = seq_el.find_mut(&format!("media/{}", kind.tag())) else { continue };
        for tn in media.children.iter_mut() {
            let Node::Element(tr) = tn else { continue };
            if tr.name != "track" {
                continue;
            }
            for cn in tr.children.iter_mut() {
                let Node::Element(ci) = cn else { continue };
                if ci.name != "clipitem" {
                    continue;
                }
                for ln in ci.children.iter_mut() {
                    let Node::Element(link) = ln else { continue };
                    if link.name != "link" {
                        continue;
                    }
                    let r = link.findtext("linkclipref").unwrap_or_default().trim().to_string();
                    if let Some(&(t, c)) = pos.get(&r) {
                        if let Some(e) = link.child_mut("trackindex") {
                            e.set_text(&t.to_string());
                        }
                        if let Some(e) = link.child_mut("clipindex") {
                            e.set_text(&c.to_string());
                        }
                    }
                }
            }
        }
    }

    // 4. sequence metadata
    let name = seq_el.findtext_nonempty("name").unwrap_or_else(|| "Sequence".into());
    seq_el.set_child_text("name", &format!("{name}{name_suffix}"));
    let end = items.iter().map(|i| i.end).max().unwrap_or(0);
    seq_el.set_child_text("duration", &end.to_string());
    seq_el.remove_child("uuid");

    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE xmeml>\n");
    out.push_str(&dom::write(&doc.root));
    std::fs::write(out_path, out).with_context(|| tr!("Não foi possível gravar {}", "Could not write {}", out_path.display()))?;
    Ok(())
}
