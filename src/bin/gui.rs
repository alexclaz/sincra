//! Desktop interface (Windows / macOS / Linux) built with egui.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

use eframe::egui::{self, Align2, Color32, FontId, RichText, Sense, Stroke, Vec2};
use sincra::engine::{mode, LogFn, ProgressFn, Status, MODES};
use sincra::media::{default_cache_dir, AudioCache};
use sincra::project::{Project, LABEL_COLORS};
use sincra::settings::Settings;
use sincra::xmeml::XmemlDocument;
use sincra::{tc, APP, VERSION};

const ORDERS: &[(&str, &str)] = &[("auto", "automática (inteligente)"), ("ctime", "por data e hora"),
                                  ("xml", "como está no XML"), ("name", "por nome de arquivo")];

fn status_color(s: Status) -> Color32 {
    match s {
        Status::Reference => Color32::from_rgb(0x4a, 0x7b, 0xd0),
        Status::Synced => Color32::from_rgb(0x3c, 0xa5, 0x5c),
        Status::ByTime => Color32::from_rgb(0xd9, 0xa5, 0x20),
        Status::Unsynced => Color32::from_rgb(0xd0, 0x50, 0x4a),
        Status::Offline => Color32::from_rgb(0x88, 0x88, 0x88),
    }
}

enum Msg {
    Log(String),
    Progress(String, f32),
    Done { project: Option<Box<Project>>, what: String, err: Option<String> },
}

/// What the window shows; kept while the project itself is inside a worker thread.
#[derive(Clone, Default)]
struct View {
    synced: bool,
    fps: f64,
    reference: Option<usize>,
    devices: Vec<DevView>,
}

#[derive(Clone)]
struct DevView {
    idx: usize,
    name: String,
    camera: bool,
    clips: Vec<ClipView>,
}

#[derive(Clone)]
struct ClipView {
    name: String,
    status: Status,
    t: Option<f64>,
    raw_zero: f64,
    duration: f64,
    score: f64,
    matched: String,
    clock: String,
}

impl View {
    fn of(p: &Project) -> View {
        View {
            synced: p.synced,
            fps: p.seq.fps,
            reference: p.opts.reference,
            devices: p.devices.iter().map(|d| DevView {
                idx: d.idx,
                name: d.name.clone(),
                camera: d.kind == "camera",
                clips: d.clips.iter().map(|&c| {
                    let c = &p.clips[c];
                    ClipView { name: c.name.clone(), status: c.status, t: c.t, raw_zero: c.raw_zero,
                               duration: if c.duration > 0.0 { c.duration } else { c.info.duration.unwrap_or(0.0) },
                               score: c.score, matched: c.matched_to.clone(), clock: c.info.ctime_source.clone().unwrap_or_default() }
                }).collect(),
            }).collect(),
        }
    }
}

struct SettingsDraft {
    s: Settings,
    cache_dir: String,
    maps: String,
    dirs: String,
}

struct App {
    store: Settings,
    project: Option<Box<Project>>,
    view: Option<View>,
    xml_path: Option<String>,
    seq_names: Vec<String>,
    seq_idx: usize,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    busy: bool,
    cancel: Arc<AtomicBool>,
    log: Vec<String>,
    stage: String,
    progress: f32,
    collapsed: HashSet<usize>,
    settings: Option<SettingsDraft>,
    pending_open: Option<String>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, open: Option<String>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let (tx, rx) = channel();
        App {
            store: Settings::load(), project: None, view: None, xml_path: None, seq_names: vec![], seq_idx: 0,
            tx, rx, busy: false, cancel: Arc::new(AtomicBool::new(false)), log: vec![], stage: "Pronto".into(),
            progress: 0.0, collapsed: HashSet::new(), settings: None, pending_open: open,
        }
    }

    fn log_fn(&self, ctx: &egui::Context) -> LogFn {
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        Arc::new(move |s: &str| {
            let _ = tx.send(Msg::Log(s.to_string()));
            ctx.request_repaint();
        })
    }

    fn progress_fn(&self, ctx: &egui::Context) -> ProgressFn {
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        Arc::new(move |s: &str, f: f64| {
            let _ = tx.send(Msg::Progress(s.to_string(), f as f32));
            ctx.request_repaint();
        })
    }

    fn error(&self, msg: &str) {
        rfd::MessageDialog::new().set_title(APP).set_level(rfd::MessageLevel::Error).set_description(msg).show();
    }

    /// Run `job` on the project in a worker thread; the project comes back with Msg::Done.
    fn run(&mut self, ctx: &egui::Context, mut p: Box<Project>, what: &str,
           job: impl FnOnce(&mut Project, ProgressFn, Arc<AtomicBool>) -> anyhow::Result<()> + Send + 'static) {
        self.cancel.store(false, Ordering::Relaxed);
        self.busy = true;
        self.view = Some(View::of(&p));
        let tx = self.tx.clone();
        let progress = self.progress_fn(ctx);
        let cancel = self.cancel.clone();
        let what = what.to_string();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&mut p, progress, cancel)));
            let err = match r {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(format!("{e:#}")),
                Err(_) => Some("erro interno (panic)".to_string()),
            };
            let _ = tx.send(Msg::Done { project: Some(p), what, err });
            ctx.request_repaint();
        });
    }

    // ------------------------------------------------------------ actions
    fn import_xml(&mut self, ctx: &egui::Context) {
        let mut d = rfd::FileDialog::new().set_title("Abrir Final Cut Pro 7 XML")
            .add_filter("Final Cut Pro 7 XML", &["xml", "XML"]).add_filter("Todos os arquivos", &["*"]);
        if !self.store.last_dir.is_empty() {
            d = d.set_directory(&self.store.last_dir);
        }
        if let Some(p) = d.pick_file() {
            self.open_path(ctx, &p.to_string_lossy());
        }
    }

    fn open_path(&mut self, ctx: &egui::Context, path: &str) {
        if self.busy {
            return;
        }
        if let Some(dir) = Path::new(path).parent() {
            self.store.last_dir = dir.to_string_lossy().into_owned();
            self.store.save();
        }
        let names = match XmemlDocument::open(path) {
            Ok(d) => d.sequence_names(),
            Err(e) => {
                self.error(&format!("XML inválido: {e:#}"));
                return;
            }
        };
        self.xml_path = Some(path.to_string());
        self.seq_names = names;
        let name = Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!("{APP} - {name}")));
        self.load_sequence(ctx, 0);
    }

    fn load_sequence(&mut self, ctx: &egui::Context, index: usize) {
        if self.busy {
            return;
        }
        let Some(path) = self.xml_path.clone() else { return };
        self.seq_idx = index;
        let p = match Project::new(&path, index, self.store.options(), self.log_fn(ctx)) {
            Ok(p) => Box::new(p),
            Err(e) => {
                self.error(&format!("XML inválido: {e:#}"));
                return;
            }
        };
        self.project = None;
        self.run(ctx, p, "XML carregado", |p, progress, _| {
            p.load(Some(progress))?;
            let missing = p.clips.iter().filter(|c| c.status == Status::Offline).count();
            if missing > 0 {
                (p.log)(&format!("[aviso] {missing} arquivo(s) não encontrado(s). Use Configurações > redirecionamento \
                                  de caminhos / pastas de busca e reimporte."));
            }
            Ok(())
        });
    }

    fn synchronize(&mut self, ctx: &egui::Context) {
        if self.busy {
            return;
        }
        let Some(mut p) = self.project.take() else { return };
        if p.synced {
            let again = rfd::MessageDialog::new().set_title(APP).set_description("A sequência já foi sincronizada. Refazer?")
                .set_buttons(rfd::MessageButtons::YesNo).show();
            if again != rfd::MessageDialogResult::Yes {
                self.project = Some(p);
                return;
            }
        }
        let reference = p.opts.reference;
        p.opts = self.store.options();
        p.opts.reference = reference;
        p.reset_sync();
        let label = mode(&p.opts.mode).map(|m| m.label).unwrap_or("");
        self.log.push(format!("--- Sincronizando ({label})"));
        self.run(ctx, p, "Sincronização concluída", |p, progress, cancel| {
            p.synchronize(Some(progress), Some(cancel))?;
            Ok(())
        });
    }

    fn stop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.log.push("Parando...".into());
    }

    fn export(&mut self) {
        let Some(p) = self.project.as_mut() else { return };
        if !p.synced {
            return;
        }
        let o = self.store.options();
        p.opts.label_synced = o.label_synced;
        p.opts.label_unsynced = o.label_unsynced;
        p.opts.suffix = o.suffix;
        let default = p.default_output();
        let mut d = rfd::FileDialog::new().set_title("Exportar XML sincronizado").add_filter("Final Cut Pro 7 XML", &["xml"]);
        if let Some(dir) = default.parent() {
            d = d.set_directory(dir);
        }
        if let Some(n) = default.file_name() {
            d = d.set_file_name(n.to_string_lossy());
        }
        let Some(mut out) = d.save_file() else { return };
        if out.extension().is_none() {
            out.set_extension("xml");
        }
        match p.export(Some(&out)) {
            Ok(out) => {
                rfd::MessageDialog::new().set_title(APP)
                    .set_description(format!("XML sincronizado salvo:\n{}\n\nImporte no Premiere / DaVinci Resolve.", out.display())).show();
            }
            Err(e) => self.error(&format!("Erro ao salvar: {e:#}")),
        }
    }

    fn set_reference(&mut self, idx: Option<usize>) {
        if let Some(p) = self.project.as_mut() {
            p.opts.reference = idx;
            self.view = Some(View::of(p));
        }
    }

    fn poll(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Log(s) => self.log.push(s),
                Msg::Progress(s, f) => {
                    self.stage = s;
                    self.progress = f;
                }
                Msg::Done { project, what, err } => {
                    self.busy = false;
                    self.project = project;
                    if let Some(p) = &self.project {
                        self.view = Some(View::of(p));
                    }
                    match err {
                        Some(e) => {
                            self.log.push(format!("[erro] {e}"));
                            self.stage = if e.contains("interrompida") { "Interrompido".into() } else { "Erro".into() };
                            if !e.contains("interrompida") {
                                self.error(&e);
                            }
                        }
                        None => {
                            self.stage = what;
                            self.progress = 1.0;
                        }
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------ settings dialog
    fn open_settings(&mut self) {
        let s = self.store.clone();
        self.settings = Some(SettingsDraft {
            cache_dir: if s.cache_dir.is_empty() { default_cache_dir().to_string_lossy().into_owned() } else { s.cache_dir.clone() },
            maps: s.path_map.iter().map(|(a, b)| format!("{a} => {b}")).collect::<Vec<_>>().join("\n"),
            dirs: s.search_dirs.join("\n"),
            s,
        });
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        let Some(draft) = self.settings.as_mut() else { return };
        let mut close = false;
        let mut save = false;
        egui::Window::new("Configurações").collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                egui::Grid::new("settings").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label("Ordem dos clipes:");
                    let cur = ORDERS.iter().find(|o| o.0 == draft.s.order).map(|o| o.1).unwrap_or("");
                    egui::ComboBox::from_id_salt("order").selected_text(cur).width(260.0).show_ui(ui, |ui| {
                        for (k, v) in ORDERS {
                            ui.selectable_value(&mut draft.s.order, k.to_string(), *v);
                        }
                    });
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(&mut draft.s.chronology, "Manter cronologia dos clipes");
                    ui.end_row();
                    for (label, val, id) in [("Label clipes sincronizados:", &mut draft.s.label_synced, "ls"),
                                             ("Label não sincronizados:", &mut draft.s.label_unsynced, "lu")] {
                        ui.label(label);
                        let shown = if val.is_empty() { "(nenhuma)".to_string() } else { val.clone() };
                        egui::ComboBox::from_id_salt(id).selected_text(shown).width(160.0).show_ui(ui, |ui| {
                            ui.selectable_value(val, String::new(), "(nenhuma)");
                            for c in LABEL_COLORS {
                                ui.selectable_value(val, c.to_string(), *c);
                            }
                        });
                        ui.end_row();
                    }
                    ui.label("Sufixo do arquivo novo:");
                    ui.text_edit_singleline(&mut draft.s.suffix);
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(&mut draft.s.use_cache, "Usar cache de áudio (acelera re-sincronizações)");
                    ui.end_row();
                    ui.label("Pasta do cache:");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut draft.cache_dir).desired_width(300.0));
                        if ui.button("...").clicked() {
                            if let Some(p) = rfd::FileDialog::new().set_title("Pasta do cache").pick_folder() {
                                draft.cache_dir = p.to_string_lossy().into_owned();
                            }
                        }
                        if ui.button("Limpar").clicked() {
                            let ok = rfd::MessageDialog::new().set_title(APP)
                                .set_description("Limpar o cache deixa mais lenta a re-sincronização de arquivos já analisados. Continuar?")
                                .set_buttons(rfd::MessageButtons::YesNo).show();
                            if ok == rfd::MessageDialogResult::Yes {
                                AudioCache::new(Some(&draft.cache_dir), true).clear();
                            }
                        }
                    });
                    ui.end_row();
                });
                ui.separator();
                ui.label("Redirecionamento de caminhos (um por linha:  antigo => novo)");
                ui.add(egui::TextEdit::multiline(&mut draft.maps).desired_rows(4).desired_width(f32::INFINITY).font(egui::TextStyle::Monospace));
                ui.label("Pastas onde procurar arquivos ausentes (uma por linha)");
                ui.add(egui::TextEdit::multiline(&mut draft.dirs).desired_rows(3).desired_width(f32::INFINITY).font(egui::TextStyle::Monospace));
                if ui.button("Adicionar pasta...").clicked() {
                    if let Some(p) = rfd::FileDialog::new().set_title("Pasta com as mídias").pick_folder() {
                        if !draft.dirs.trim().is_empty() {
                            draft.dirs.push('\n');
                        }
                        draft.dirs.push_str(&p.to_string_lossy());
                    }
                }
                ui.add_space(8.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Cancelar").clicked() {
                        close = true;
                    }
                    if ui.button("OK").clicked() {
                        save = true;
                    }
                });
            });
        if save {
            let mut s = draft.s.clone();
            s.cache_dir = if draft.cache_dir == default_cache_dir().to_string_lossy() { String::new() } else { draft.cache_dir.clone() };
            s.path_map = draft.maps.lines().filter_map(|l| l.split_once("=>"))
                .map(|(a, b)| (a.trim().to_string(), b.trim().to_string())).filter(|(a, b)| !a.is_empty() && !b.is_empty()).collect();
            s.search_dirs = draft.dirs.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect();
            s.mode = self.store.mode.clone();
            s.last_dir = self.store.last_dir.clone();
            self.store = s;
            self.store.save();
            close = true;
        }
        if close {
            self.settings = None;
        }
    }

    // ------------------------------------------------------------ view
    fn timeline(&self, ui: &mut egui::Ui) {
        const LANE: f32 = 26.0;
        let ndev = self.view.as_ref().map(|v| v.devices.len()).unwrap_or(0);
        let h = (14.0 + LANE * ndev as f32).max(60.0);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, Color32::from_rgb(0x20, 0x23, 0x2a));
        let Some(v) = self.view.as_ref().filter(|v| !v.devices.is_empty()) else {
            painter.text(rect.left_top() + Vec2::new(12.0, 20.0), Align2::LEFT_CENTER, "Importe um XML para ver a timeline",
                         FontId::proportional(13.0), Color32::from_rgb(0x8a, 0x8f, 0x98));
            return;
        };
        let spans: Vec<(f64, f64)> = v.devices.iter().flat_map(|d| d.clips.iter()).map(|c| (c.t.unwrap_or(c.raw_zero), c.duration)).collect();
        let t0 = spans.iter().map(|s| s.0).fold(f64::INFINITY, f64::min);
        let t1 = spans.iter().map(|s| s.0 + s.1).fold(f64::NEG_INFINITY, f64::max);
        let w = (rect.width() as f64 - 110.0).max(100.0);
        let k = w / (t1 - t0).max(1e-6);
        for (i, d) in v.devices.iter().enumerate() {
            let y = rect.top() + 8.0 + i as f32 * LANE;
            painter.text(egui::pos2(rect.left() + 6.0, y + 9.0), Align2::LEFT_CENTER, &d.name, FontId::proportional(12.0),
                         Color32::from_rgb(0xc9, 0xcc, 0xd1));
            for c in &d.clips {
                let s = c.t.unwrap_or(c.raw_zero);
                let x0 = rect.left() + 100.0 + ((s - t0) * k) as f32;
                let x1 = (x0 + 2.0).max(x0 + (c.duration * k) as f32);
                let col = if v.synced || c.status == Status::Offline { status_color(c.status) } else { Color32::from_rgb(0x5b, 0x62, 0x70) };
                let r = egui::Rect::from_min_max(egui::pos2(x0, y), egui::pos2(x1, y + 18.0));
                painter.rect(r, 2.0, col, Stroke::new(1.0_f32, Color32::from_rgb(0x15, 0x17, 0x1c)));
            }
        }
    }

    fn table(&mut self, ui: &mut egui::Ui) {
        let Some(v) = self.view.clone() else {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| ui.label(RichText::new("Clique em IMPORTAR XML ou arraste o arquivo para cá").weak()));
            return;
        };
        let mut set_ref: Option<Option<usize>> = None;
        egui::ScrollArea::both().auto_shrink([false, false]).id_salt("table").show(ui, |ui| {
            egui::Grid::new("clips").num_columns(6).striped(true).spacing([18.0, 4.0]).min_col_width(60.0).show(ui, |ui| {
                for h in ["Dispositivo / clipe", "Status", "Posição na timeline", "Confiança", "Casado com", "Fonte do horário"] {
                    ui.label(RichText::new(h).strong());
                }
                ui.end_row();
                for d in &v.devices {
                    let open = !self.collapsed.contains(&d.idx);
                    let kind = if d.camera { "câmera" } else { "gravador" };
                    let r = if v.reference == Some(d.idx) { " - REFERÊNCIA" } else { "" };
                    let text = format!("{} {} ({kind}, {} clipes){r}", if open { "⏷" } else { "⏵" }, d.name, d.clips.len());
                    let resp = ui.add(egui::Label::new(RichText::new(text).strong()).sense(Sense::click()));
                    if resp.clicked() {
                        if open {
                            self.collapsed.insert(d.idx);
                        } else {
                            self.collapsed.remove(&d.idx);
                        }
                    }
                    if !self.busy {
                        resp.context_menu(|ui| {
                            if ui.button("Usar como referência").clicked() {
                                set_ref = Some(Some(d.idx));
                                ui.close_menu();
                            }
                            if ui.button("Referência automática").clicked() {
                                set_ref = Some(None);
                                ui.close_menu();
                            }
                        });
                    }
                    ui.end_row();
                    if !open {
                        continue;
                    }
                    for c in &d.clips {
                        let show = v.synced || c.status == Status::Offline;
                        let col = if show { status_color(c.status) } else { ui.visuals().text_color() };
                        ui.label(RichText::new(format!("    {}", c.name)).color(col));
                        ui.label(RichText::new(if show { c.status.pt() } else { "-" }).color(col));
                        ui.label(if v.synced { tc(c.t, v.fps) } else { "-".into() });
                        ui.label(if c.score != 0.0 { format!("{:.1}", c.score) } else { String::new() });
                        ui.label(&c.matched);
                        ui.label(&c.clock);
                        ui.end_row();
                    }
                }
            });
        });
        if let Some(r) = set_ref {
            self.set_reference(r);
        }
    }

    fn summary(&self) -> String {
        let Some(v) = &self.view else { return String::new() };
        let n: usize = v.devices.iter().map(|d| d.clips.len()).sum();
        if v.synced {
            let ok = v.devices.iter().flat_map(|d| &d.clips).filter(|c| c.status.placed()).count();
            format!("{ok}/{n} clipes sincronizados pelo áudio")
        } else {
            let cams = v.devices.iter().filter(|d| d.camera).count();
            format!("{n} clipes, {cams} câmera(s), {} gravador(es)", v.devices.len() - cams)
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        if let Some(p) = self.pending_open.take() {
            self.open_path(ctx, &p);
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        if let Some(p) = dropped.first() {
            self.open_path(ctx, &p.to_string_lossy());
        }
        let has = self.project.is_some();
        let synced = self.project.as_ref().is_some_and(|p| p.synced);

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(!self.busy, egui::Button::new(RichText::new("IMPORTAR XML").strong())).clicked() {
                    self.import_xml(ctx);
                }
                ui.label("  Sequência:");
                let cur = self.seq_names.get(self.seq_idx).cloned().unwrap_or_default();
                let mut pick = None;
                ui.add_enabled_ui(!self.busy && !self.seq_names.is_empty(), |ui| {
                    egui::ComboBox::from_id_salt("seq").selected_text(cur).width(220.0).show_ui(ui, |ui| {
                        for (i, n) in self.seq_names.iter().enumerate() {
                            if ui.selectable_label(i == self.seq_idx, n).clicked() {
                                pick = Some(i);
                            }
                        }
                    });
                });
                if let Some(i) = pick.filter(|&i| i != self.seq_idx) {
                    self.load_sequence(ctx, i);
                }
                ui.label("  Modo:");
                let cur = mode(&self.store.mode).map(|m| m.label).unwrap_or("");
                let before = self.store.mode.clone();
                egui::ComboBox::from_id_salt("mode").selected_text(cur).width(380.0).show_ui(ui, |ui| {
                    for m in MODES {
                        ui.selectable_value(&mut self.store.mode, m.name.to_string(), m.label);
                    }
                });
                if self.store.mode != before {
                    self.store.save();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Configurações...").clicked() {
                        self.open_settings();
                    }
                });
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(has && !self.busy, egui::Button::new(RichText::new("SINCRONIZAR").strong())).clicked() {
                    self.synchronize(ctx);
                }
                if ui.add_enabled(self.busy, egui::Button::new("Parar")).clicked() {
                    self.stop();
                }
                if ui.add_enabled(synced && !self.busy, egui::Button::new(RichText::new("EXPORTAR XML").strong())).clicked() {
                    self.export();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(self.summary());
                });
            });
            ui.add_space(6.0);
            self.timeline(ui);
            ui.add_space(6.0);
        });

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_sized([220.0, 18.0], egui::Label::new(&self.stage).truncate());
                ui.add(egui::ProgressBar::new(self.progress.clamp(0.0, 1.0)).desired_height(14.0));
            });
            ui.add_space(4.0);
        });

        egui::TopBottomPanel::bottom("log").resizable(true).default_height(150.0).min_height(60.0).show(ctx, |ui| {
            egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                for l in &self.log {
                    let col = if l.starts_with("[erro]") {
                        Color32::from_rgb(0xe0, 0x60, 0x5a)
                    } else if l.starts_with("[aviso]") {
                        Color32::from_rgb(0xd9, 0xa5, 0x20)
                    } else {
                        ui.visuals().text_color()
                    };
                    ui.label(RichText::new(l).monospace().size(12.0).color(col));
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| self.table(ui));

        self.settings_window(ctx);
        if self.busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

fn main() -> eframe::Result<()> {
    let open = std::env::args().nth(1).filter(|a| Path::new(a).exists());
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!("{APP} {VERSION}"))
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([820.0, 520.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(APP, options, Box::new(move |cc| Ok(Box::new(App::new(cc, open)))))
}
