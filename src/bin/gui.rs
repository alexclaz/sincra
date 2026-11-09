//! Desktop interface (Windows / macOS / Linux) built with egui.
//!
//! Layout, NLE-style: a toolbar, the timeline as the main view (track headers on the left), a clip
//! inspector on the right and a status bar with the last log line (click Log for the full history).
//! Portuguese / English, light / dark (both follow the system unless chosen in the settings).
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

use eframe::egui::{self, Align, Align2, Color32, FontFamily, FontId, Layout, Margin, Pos2, Rect, RichText, Rounding, Sense,
                   Shape, Stroke, TextStyle, Vec2};
use sincra::engine::{Cancelled, LogFn, ProgressFn, Status, MODES, PEAKS_PER_SEC};
use sincra::media::{default_cache_dir, AudioCache};
use sincra::project::{Project, LABEL_COLORS};
use sincra::settings::Settings;
use sincra::xmeml::XmemlDocument;
use sincra::{tc, tr, APP, VERSION};

// ------------------------------------------------------------------ palettes

struct Pal {
    bg: Color32,     // timeline
    panel: Color32,  // toolbar, headers, inspector
    raised: Color32, // buttons, inputs, selected header
    hover: Color32,
    head_hover: Color32,
    line: Color32, // separators
    grid: Color32,
    text: Color32,
    dim: Color32,
    faint: Color32,
    accent: Color32,
    on_accent: Color32,
    devices: [Color32; 8],
    reference: Color32,
    synced: Color32,
    by_time: Color32,
    unsynced: Color32,
    offline: Color32,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

static DARK_PAL: Pal = Pal {
    bg: rgb(0x0f, 0x11, 0x14),
    panel: rgb(0x17, 0x1a, 0x1e),
    raised: rgb(0x20, 0x24, 0x29),
    hover: rgb(0x29, 0x2e, 0x34),
    head_hover: rgb(0x1b, 0x1f, 0x23),
    line: rgb(0x08, 0x09, 0x0b),
    grid: rgb(0x19, 0x1c, 0x20),
    text: rgb(0xd8, 0xdb, 0xdf),
    dim: rgb(0x8a, 0x90, 0x98),
    faint: rgb(0x55, 0x5b, 0x63),
    accent: rgb(0xe8, 0x93, 0x2a),
    on_accent: rgb(0x1c, 0x12, 0x02),
    // muted clip colours, kept away from the status hues (green / amber / red)
    devices: [rgb(0x5b, 0x8b, 0xd9), rgb(0x3f, 0xa8, 0x9b), rgb(0x95, 0x7f, 0xd6), rgb(0x4f, 0xa6, 0xc9),
              rgb(0xc4, 0x7a, 0xa5), rgb(0x7f, 0x8f, 0xb8), rgb(0x6c, 0xa3, 0x7c), rgb(0xa8, 0x8f, 0x6e)],
    reference: rgb(0x8e, 0xb4, 0xf0),
    synced: rgb(0x6c, 0xc0, 0x7a),
    by_time: rgb(0xd8, 0xa6, 0x45),
    unsynced: rgb(0xe0, 0x6c, 0x64),
    offline: rgb(0x7a, 0x80, 0x88),
};

static LIGHT_PAL: Pal = Pal {
    bg: rgb(0xe8, 0xea, 0xed),
    panel: rgb(0xf6, 0xf7, 0xf8),
    raised: rgb(0xe2, 0xe5, 0xe9),
    hover: rgb(0xd7, 0xdb, 0xe0),
    head_hover: rgb(0xee, 0xf0, 0xf2),
    line: rgb(0xcc, 0xd0, 0xd6),
    grid: rgb(0xdc, 0xdf, 0xe3),
    text: rgb(0x1f, 0x23, 0x28),
    dim: rgb(0x58, 0x60, 0x6a),
    faint: rgb(0x92, 0x99, 0xa1),
    accent: rgb(0xe0, 0x87, 0x1a),
    on_accent: rgb(0x1c, 0x12, 0x02),
    devices: [rgb(0x47, 0x74, 0xc2), rgb(0x2b, 0x8e, 0x81), rgb(0x7c, 0x64, 0xc2), rgb(0x34, 0x8c, 0xb0),
              rgb(0xab, 0x5d, 0x8b), rgb(0x63, 0x73, 0x9b), rgb(0x52, 0x88, 0x62), rgb(0x8d, 0x72, 0x53)],
    reference: rgb(0x34, 0x63, 0xb4),
    synced: rgb(0x2b, 0x86, 0x3f),
    by_time: rgb(0xa3, 0x6f, 0x0c),
    unsynced: rgb(0xbf, 0x42, 0x39),
    offline: rgb(0x78, 0x7e, 0x86),
};

static IS_DARK: AtomicBool = AtomicBool::new(true);

fn p() -> &'static Pal {
    if IS_DARK.load(Ordering::Relaxed) {
        &DARK_PAL
    } else {
        &LIGHT_PAL
    }
}

fn is_dark() -> bool {
    IS_DARK.load(Ordering::Relaxed)
}

fn device_color(i: usize) -> Color32 {
    p().devices[i % 8]
}

fn status_color(s: Status) -> Color32 {
    let p = p();
    match s {
        Status::Reference => p.reference,
        Status::Synced => p.synced,
        Status::ByTime => p.by_time,
        Status::Unsynced => p.unsynced,
        Status::Offline => p.offline,
    }
}

fn alpha(c: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

fn ui_font(size: f32) -> FontId {
    FontId::proportional(size)
}

/// Fonts are bundled (Inter + JetBrains Mono, SIL OFL) so the interface looks the same on every system:
/// system UI fonts rendered without hinting look uneven at small sizes.
fn setup_fonts(ctx: &egui::Context) {
    let mut f = egui::FontDefinitions::default();
    f.font_data.insert("inter".into(), egui::FontData::from_static(include_bytes!("../../assets/fonts/Inter-Regular.ttf")));
    f.font_data.insert("inter-semibold".into(), egui::FontData::from_static(include_bytes!("../../assets/fonts/Inter-SemiBold.ttf")));
    f.font_data.insert("jetbrains-mono".into(), egui::FontData::from_static(include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf")));
    // egui's own fonts stay as fallbacks (symbols such as the gear)
    f.families.get_mut(&FontFamily::Proportional).unwrap().insert(0, "inter".into());
    f.families.get_mut(&FontFamily::Monospace).unwrap().insert(0, "jetbrains-mono".into());
    let mut list = f.families[&FontFamily::Proportional].clone();
    list.insert(0, "inter-semibold".into());
    f.families.insert(semibold(), list);
    ctx.set_fonts(f);
}

fn visuals_for(pal: &Pal, dark: bool) -> egui::Visuals {
    let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.panel_fill = pal.panel;
    v.window_fill = pal.panel;
    v.extreme_bg_color = if dark { pal.bg } else { rgb(0xff, 0xff, 0xff) };
    v.faint_bg_color = pal.raised;
    v.window_stroke = Stroke::new(1.0_f32, pal.line);
    v.window_rounding = Rounding::same(4.0);
    v.menu_rounding = Rounding::same(4.0);
    v.window_shadow = egui::epaint::Shadow { offset: Vec2::new(0.0, 6.0), blur: 18.0, spread: 0.0,
                                             color: alpha(Color32::BLACK, if dark { 120 } else { 40 }) };
    v.popup_shadow = v.window_shadow;
    v.hyperlink_color = pal.accent;
    v.selection.bg_fill = alpha(pal.accent, if dark { 60 } else { 80 });
    v.selection.stroke = Stroke::new(1.0_f32, pal.accent);
    let w = &mut v.widgets;
    w.noninteractive.bg_stroke = Stroke::new(1.0_f32, pal.line);
    w.noninteractive.fg_stroke = Stroke::new(1.0_f32, pal.text);
    w.noninteractive.bg_fill = pal.panel;
    for (st, fill) in [(&mut w.inactive, pal.raised), (&mut w.hovered, pal.hover), (&mut w.active, pal.hover), (&mut w.open, pal.hover)] {
        st.bg_fill = fill;
        st.weak_bg_fill = fill;
        st.bg_stroke = Stroke::NONE;
        st.fg_stroke = Stroke::new(1.0_f32, pal.text);
        st.rounding = Rounding::same(3.0);
        st.expansion = 0.0;
    }
    v
}

fn setup_style(ctx: &egui::Context) {
    ctx.set_visuals_of(egui::Theme::Dark, visuals_for(&DARK_PAL, true));
    ctx.set_visuals_of(egui::Theme::Light, visuals_for(&LIGHT_PAL, false));
    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        ctx.style_mut_of(theme, |s| {
            s.spacing.item_spacing = Vec2::new(6.0, 4.0);
            s.spacing.button_padding = Vec2::new(10.0, 4.0);
            s.spacing.interact_size.y = 26.0;
            s.spacing.combo_height = 320.0;
            s.text_styles.insert(TextStyle::Small, ui_font(11.0));
            s.text_styles.insert(TextStyle::Body, ui_font(13.0));
            s.text_styles.insert(TextStyle::Button, ui_font(13.0));
            s.text_styles.insert(TextStyle::Heading, FontId::new(15.0, semibold()));
            s.text_styles.insert(TextStyle::Monospace, FontId::monospace(12.0));
        });
    }
}

fn theme_pref(code: &str) -> egui::ThemePreference {
    match code {
        "dark" => egui::ThemePreference::Dark,
        "light" => egui::ThemePreference::Light,
        _ => egui::ThemePreference::System,
    }
}

fn app_icon() -> egui::IconData {
    // same artwork as the executables (packaging/icon/make_icon.py)
    eframe::icon_data::from_png_bytes(include_bytes!("../../packaging/icon/sincra-256.png")).unwrap_or_default()
}

// ------------------------------------------------------------------ helpers

fn primary_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).color(p().on_accent).family(semibold())).fill(p().accent)
        .min_size(Vec2::new(0.0, 28.0))
}

fn tool_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).color(p().text)).min_size(Vec2::new(0.0, 28.0))
}

fn short_mode(name: &str) -> &'static str {
    match name {
        "approximate" => tr!("Aproximado", "Approximate"),
        "very_fast" => tr!("Muito rápido", "Very fast"),
        "fast" => tr!("Rápido", "Fast"),
        "advanced" => tr!("Avançado", "Advanced"),
        "optimal" => tr!("Ótimo", "Optimal"),
        "thorough" => tr!("Minucioso", "Thorough"),
        "utmost" => tr!("Máximo", "Utmost"),
        _ => "?",
    }
}

fn mode_hint(name: &str) -> &'static str {
    match name {
        "approximate" => tr!("Só pelo horário de criação dos arquivos, sem ler o áudio. Erro de ~1 s.",
                             "Creation times only, no audio analysis. About 1 s of error."),
        "very_fast" => tr!("Uma passada grossa pelo áudio, sem refinamento.", "One coarse pass over the audio, no refinement."),
        "fast" => tr!("Passada grossa + refinamento fino.", "Coarse pass + fine refinement."),
        "advanced" => tr!("Várias passadas, sem usar o relógio das câmeras.", "Several passes, without using the camera clocks."),
        "optimal" => tr!("Várias passadas, usando o horário de criação para prever onde procurar. Para mídia original.",
                         "Several passes, using creation times to predict where to search. For original media."),
        "thorough" => tr!("Como o Ótimo, sem confiar no relógio. Para proxies e arquivos baixados.",
                          "Like Optimal, without trusting the clocks. For proxies and downloaded files."),
        "utmost" => tr!("Minucioso + várias resoluções de análise. O mais lento.", "Thorough + several analysis resolutions. The slowest."),
        _ => "",
    }
}

fn order_label(code: &str) -> &'static str {
    match code {
        "ctime" => tr!("Data e hora", "Date and time"),
        "xml" => tr!("Como está no XML", "As in the XML"),
        "name" => tr!("Nome do arquivo", "File name"),
        _ => tr!("Automática", "Automatic"),
    }
}

fn ruler_label(t: f64, long: bool) -> String {
    let s = t.max(0.0).round() as i64;
    if long {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

/// Signed offset: "+3.2 s", "-12:03.5", "+1:02:03" (decimal comma in Portuguese).
fn fmt_delta(d: f64) -> String {
    let sign = if d < 0.0 { "-" } else { "+" };
    let a = d.abs();
    let s = if a < 60.0 {
        format!("{sign}{:.1} s", a)
    } else if a < 3600.0 {
        format!("{sign}{}:{:04.1}", (a / 60.0) as i64, a % 60.0)
    } else {
        let s = a.round() as i64;
        format!("{sign}{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    };
    if sincra::i18n::is_en() { s } else { s.replace('.', ",") }
}

fn dashed_rect(painter: &egui::Painter, r: Rect, stroke: Stroke) {
    let pts = vec![r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
    painter.extend(Shape::dashed_line(&pts, stroke, 4.0, 3.0));
}

fn log_color(l: &str) -> Color32 {
    if l.starts_with("[erro]") || l.starts_with("[error]") {
        p().unsynced
    } else if l.starts_with("[aviso]") || l.starts_with("[warning]") {
        p().by_time
    } else {
        p().dim
    }
}

// ------------------------------------------------------------------ state

enum Msg {
    Log(String),
    Progress(String, f32),
    Done { project: Option<Box<Project>>, what: &'static str, err: Option<String>, cancelled: bool },
}

/// What the window shows; kept while the project itself is inside a worker thread.
#[derive(Clone, Default)]
struct View {
    synced: bool,
    fps: f64,
    reference: Option<usize>,
    span: f64,
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
    path: String,
    status: Status,
    start: f64, // position on the timeline shown (s)
    orig: f64,  // position on the imported timeline (s)
    duration: f64,
    score: f64,
    matched: String,
    clock: String,
    peaks: Option<Arc<Vec<f32>>>,
    peak_max: f32,
}

impl ClipView {
    fn delta(&self) -> f64 {
        self.start - self.orig
    }
}

impl View {
    fn of(proj: &Project) -> View {
        let pos = |c: &sincra::engine::Clip| c.t.unwrap_or(c.raw_zero);
        let fold_min = |it: &mut dyn Iterator<Item = f64>| {
            let m = it.fold(f64::INFINITY, f64::min);
            if m.is_finite() { m } else { 0.0 }
        };
        let t0 = fold_min(&mut proj.clips.iter().map(pos));
        let raw0 = fold_min(&mut proj.clips.iter().map(|c| c.raw_zero)).min(0.0);
        let mut span: f64 = 0.0;
        let devices = proj.devices.iter().map(|d| DevView {
            idx: d.idx,
            name: d.name.clone(),
            camera: d.kind == "camera",
            clips: d.clips.iter().map(|&c| {
                let c = &proj.clips[c];
                let duration = if c.duration > 0.0 { c.duration } else { c.info.duration.unwrap_or(0.0) };
                let start = pos(c) - t0;
                let orig = c.raw_zero - raw0;
                span = span.max(start + duration).max(orig + duration);
                let peak_max = c.peaks.as_ref().map(|pk| pk.iter().cloned().fold(0f32, f32::max)).unwrap_or(0.0);
                ClipView { name: c.name.clone(), path: c.path.clone(), status: c.status, start, orig, duration, score: c.score,
                           matched: c.matched_to.clone(), clock: c.info.ctime_source.clone().unwrap_or_default(),
                           peaks: c.peaks.clone(), peak_max }
            }).collect(),
        }).collect();
        View { synced: proj.synced, fps: proj.seq.fps, reference: proj.opts.reference, span, devices }
    }

    /// Device that served as the reference: the forced one, or after a sync the one holding reference clips.
    fn reference_device(&self) -> Option<usize> {
        self.reference.or_else(|| {
            self.devices.iter().find(|d| d.clips.iter().any(|c| c.status == Status::Reference)).map(|d| d.idx)
        })
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Selection {
    None,
    Device(usize),
    Clip(usize, usize),
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
    settings: Option<SettingsDraft>,
    pending_open: Option<String>,
    sel: Selection,
    px_per_sec: Option<f64>,
    offset: f64,
    log_open: bool,
    show_original: bool,
    /// For documentation screenshots: SINCRA_VIEW="start,end" (seconds) and SINCRA_SELECT=<track> are
    /// applied once, right after the first synchronisation.
    demo: Option<(Option<(f64, f64)>, Option<usize>)>,
}

fn demo_from_env() -> Option<(Option<(f64, f64)>, Option<usize>)> {
    let view = std::env::var("SINCRA_VIEW").ok().and_then(|v| {
        let (a, b) = v.split_once(',')?;
        Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
    });
    let sel = std::env::var("SINCRA_SELECT").ok().and_then(|v| v.trim().parse().ok());
    (view.is_some() || sel.is_some()).then_some((view, sel))
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, open: Option<String>) -> Self {
        let store = Settings::load();
        sincra::i18n::set_lang(&store.language);
        setup_fonts(&cc.egui_ctx);
        setup_style(&cc.egui_ctx);
        cc.egui_ctx.set_theme(theme_pref(&store.theme));
        let (tx, rx) = channel();
        App {
            store, project: None, view: None, xml_path: None, seq_names: vec![], seq_idx: 0,
            tx, rx, busy: false, cancel: Arc::new(AtomicBool::new(false)), log: vec![], stage: String::new(),
            progress: 0.0, settings: None, pending_open: open, sel: Selection::None, px_per_sec: None, offset: 0.0,
            log_open: false, show_original: false, demo: demo_from_env(),
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
    fn run(&mut self, ctx: &egui::Context, mut proj: Box<Project>, what: &'static str,
           job: impl FnOnce(&mut Project, ProgressFn, Arc<AtomicBool>) -> anyhow::Result<()> + Send + 'static) {
        self.cancel.store(false, Ordering::Relaxed);
        self.busy = true;
        self.progress = 0.0;
        self.view = Some(View::of(&proj));
        let tx = self.tx.clone();
        let progress = self.progress_fn(ctx);
        let cancel = self.cancel.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&mut proj, progress, cancel)));
            let (err, cancelled) = match r {
                Ok(Ok(())) => (None, false),
                Ok(Err(e)) => (Some(format!("{e:#}")), e.is::<Cancelled>()),
                Err(_) => (Some(tr!("erro interno (panic)", "internal error (panic)").to_string()), false),
            };
            let _ = tx.send(Msg::Done { project: Some(proj), what, err, cancelled });
            ctx.request_repaint();
        });
    }

    // ------------------------------------------------------------ actions
    fn import_xml(&mut self, ctx: &egui::Context) {
        let mut d = rfd::FileDialog::new().set_title(tr!("Importar XML (Final Cut Pro 7)", "Import XML (Final Cut Pro 7)"))
            .add_filter("Final Cut Pro 7 XML", &["xml", "XML"]).add_filter(tr!("Todos os arquivos", "All files"), &["*"]);
        if !self.store.last_dir.is_empty() {
            d = d.set_directory(&self.store.last_dir);
        }
        if let Some(path) = d.pick_file() {
            self.open_path(ctx, &path.to_string_lossy());
        }
    }

    fn open_path(&mut self, ctx: &egui::Context, path: &str) {
        if self.busy {
            return;
        }
        let names = match XmemlDocument::open(path) {
            Ok(d) => d.sequence_names(),
            Err(e) => {
                self.error(&tr!("XML inválido: {:#}", "Invalid XML: {:#}", e));
                return;
            }
        };
        if let Some(dir) = Path::new(path).parent() {
            self.store.last_dir = dir.to_string_lossy().into_owned();
        }
        self.store.add_recent(path);
        self.store.save();
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
        let proj = match Project::new(&path, index, self.store.options(), self.log_fn(ctx)) {
            Ok(x) => Box::new(x),
            Err(e) => {
                self.error(&tr!("XML inválido: {:#}", "Invalid XML: {:#}", e));
                return;
            }
        };
        self.project = None;
        self.sel = Selection::None;
        self.px_per_sec = None;
        self.offset = 0.0;
        self.run(ctx, proj, tr!("XML carregado", "XML loaded"), |proj, progress, _| {
            proj.load(Some(progress))?;
            let missing = proj.clips.iter().filter(|c| c.status == Status::Offline).count();
            if missing > 0 {
                (proj.log)(&tr!("[aviso] {} arquivo(s) não encontrado(s). Use Configurações > Mídia que mudou de lugar e reimporte.",
                                "[warning] {} file(s) not found. Use Settings > Media that moved and import again.", missing));
            }
            Ok(())
        });
    }

    fn synchronize(&mut self, ctx: &egui::Context) {
        if self.busy {
            return;
        }
        let Some(mut proj) = self.project.take() else { return };
        if proj.synced {
            let again = rfd::MessageDialog::new().set_title(APP)
                .set_description(tr!("A sequência já foi sincronizada. Refazer?", "This sequence is already synced. Run again?"))
                .set_buttons(rfd::MessageButtons::YesNo).show();
            if again != rfd::MessageDialogResult::Yes {
                self.project = Some(proj);
                return;
            }
        }
        let reference = proj.opts.reference;
        proj.opts = self.store.options();
        proj.opts.reference = reference;
        proj.reset_sync();
        self.log.push(format!("--- {} ({})", tr!("Sincronizando", "Synchronising"), short_mode(&proj.opts.mode)));
        self.px_per_sec = None;
        self.run(ctx, proj, tr!("Sincronização concluída", "Synchronisation finished"), |proj, progress, cancel| {
            proj.synchronize(Some(progress), Some(cancel))?;
            Ok(())
        });
    }

    fn stop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.log.push(tr!("Parando...", "Stopping...").into());
    }

    fn export(&mut self) {
        let Some(proj) = self.project.as_mut() else { return };
        if !proj.synced {
            return;
        }
        let o = self.store.options();
        proj.opts.label_synced = o.label_synced;
        proj.opts.label_unsynced = o.label_unsynced;
        proj.opts.suffix = o.suffix;
        let default = proj.default_output();
        let mut d = rfd::FileDialog::new().set_title(tr!("Exportar XML sincronizado", "Export synced XML"))
            .add_filter("Final Cut Pro 7 XML", &["xml"]);
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
        match proj.export(Some(&out)) {
            Ok(out) => {
                rfd::MessageDialog::new().set_title(APP).set_description(tr!(
                    "XML sincronizado salvo:\n{}\n\nImporte no DaVinci Resolve / Premiere.",
                    "Synced XML saved:\n{}\n\nImport it in DaVinci Resolve / Premiere.", out.display())).show();
            }
            Err(e) => self.error(&tr!("Erro ao salvar: {:#}", "Could not save: {:#}", e)),
        }
    }

    fn toggle_reference(&mut self, idx: usize) {
        if self.busy {
            return;
        }
        if let Some(proj) = self.project.as_mut() {
            proj.opts.reference = if proj.opts.reference == Some(idx) { None } else { Some(idx) };
            self.view = Some(View::of(proj));
        }
    }

    fn set_theme(&mut self, ctx: &egui::Context, code: &str) {
        self.store.theme = code.to_string();
        self.store.save();
        ctx.set_theme(theme_pref(code));
    }

    fn poll(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Log(s) => self.log.push(s),
                Msg::Progress(s, f) => {
                    self.stage = s;
                    self.progress = f;
                }
                Msg::Done { project, what, err, cancelled } => {
                    self.busy = false;
                    self.project = project;
                    if let Some(proj) = &self.project {
                        self.view = Some(View::of(proj));
                        if proj.synced {
                            self.show_original = false;
                        }
                    }
                    match err {
                        Some(_) if cancelled => {
                            // the user pressed Stop: not an error
                            self.log.push(tr!("Sincronização interrompida.", "Synchronisation stopped.").into());
                            self.stage = tr!("Interrompido", "Stopped").into();
                        }
                        Some(e) => {
                            self.log.push(format!("{} {e}", tr!("[erro]", "[error]")));
                            self.stage = tr!("Erro", "Error").into();
                            self.error(&e);
                        }
                        None => {
                            self.stage = what.into();
                            self.progress = 1.0;
                        }
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------ toolbar
    fn toolbar(&mut self, ctx: &egui::Context) {
        let has = self.project.is_some();
        let synced = self.project.as_ref().is_some_and(|x| x.synced);
        egui::TopBottomPanel::top("toolbar").exact_height(44.0).show_separator_line(false)
            .frame(egui::Frame::none().fill(p().panel).inner_margin(Margin::symmetric(12.0, 0.0)))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(RichText::new(APP).family(semibold()).size(14.0).color(p().text));
                    if let Some(path) = &self.xml_path {
                        let name = Path::new(path).file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        ui.label(RichText::new("/").color(p().faint));
                        ui.label(RichText::new(name).color(p().dim)).on_hover_text(path.as_str());
                    }
                    if self.seq_names.len() > 1 {
                        let cur = self.seq_names.get(self.seq_idx).cloned().unwrap_or_default();
                        let mut pick = None;
                        ui.add_enabled_ui(!self.busy, |ui| {
                            egui::ComboBox::from_id_salt("seq").selected_text(cur).width(180.0).show_ui(ui, |ui| {
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
                    }
                    ui.add_space(14.0);
                    if ui.add_enabled(!self.busy, tool_button(tr!("Importar XML", "Import XML"))).clicked() {
                        self.import_xml(ctx);
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add(egui::Button::new(RichText::new("⚙").size(15.0).color(p().dim)).frame(false))
                            .on_hover_text(tr!("Configurações", "Settings")).clicked() {
                            self.open_settings();
                        }
                        // light / dark toggle: a half-filled circle
                        let (r, resp) = ui.allocate_exact_size(Vec2::new(24.0, 24.0), Sense::click());
                        let col = if resp.hovered() { p().text } else { p().dim };
                        let c = r.center();
                        ui.painter().circle_stroke(c, 6.5, Stroke::new(1.4_f32, col));
                        let half: Vec<Pos2> = (0..=16).map(|k| {
                            let a = std::f32::consts::PI * (0.5 + k as f32 / 16.0);
                            c + Vec2::new(a.cos(), a.sin()) * 6.5
                        }).collect();
                        ui.painter().add(Shape::convex_polygon(half, col, Stroke::NONE));
                        let tip = if is_dark() { tr!("Tema claro", "Light theme") } else { tr!("Tema escuro", "Dark theme") };
                        if resp.on_hover_text(tip).clicked() {
                            let next = if is_dark() { "light" } else { "dark" };
                            self.set_theme(ctx, next);
                        }
                        ui.add_space(6.0);
                        if ui.add_enabled(synced && !self.busy, tool_button(tr!("Exportar XML", "Export XML"))).clicked() {
                            self.export();
                        }
                        if self.busy {
                            if ui.add(tool_button(tr!("Parar", "Stop"))).clicked() {
                                self.stop();
                            }
                        } else if ui.add_enabled(has, primary_button(tr!("Sincronizar", "Sync"))).clicked() {
                            self.synchronize(ctx);
                        }
                        let before = self.store.mode.clone();
                        ui.add_enabled_ui(!self.busy, |ui| {
                            egui::ComboBox::from_id_salt("mode").selected_text(short_mode(&self.store.mode)).width(120.0)
                                .show_ui(ui, |ui| {
                                    for m in MODES {
                                        ui.selectable_value(&mut self.store.mode, m.name.to_string(), short_mode(m.name))
                                            .on_hover_text(mode_hint(m.name));
                                    }
                                }).response.on_hover_text(mode_hint(&self.store.mode));
                        });
                        if self.store.mode != before {
                            self.store.save();
                        }
                        ui.label(RichText::new(tr!("Precisão", "Precision")).color(p().dim));
                    });
                });
            });
    }

    // ------------------------------------------------------------ status bar + log drawer
    fn status_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status").exact_height(26.0).show_separator_line(false)
            .frame(egui::Frame::none().fill(p().panel).inner_margin(Margin::symmetric(12.0, 0.0)))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    if self.busy {
                        ui.add(egui::Spinner::new().size(12.0).color(p().accent));
                        ui.label(RichText::new(&self.stage).size(12.0).color(p().text));
                        let (r, _) = ui.allocate_exact_size(Vec2::new(140.0, 4.0), Sense::hover());
                        ui.painter().rect_filled(r, 2.0, p().raised);
                        let mut done = r;
                        done.set_width(r.width() * self.progress.clamp(0.0, 1.0));
                        ui.painter().rect_filled(done, 2.0, p().accent);
                        ui.label(RichText::new(format!("{:.0}%", self.progress * 100.0)).size(12.0).color(p().dim));
                    } else {
                        let last = self.log.last().cloned().unwrap_or_else(|| tr!("Pronto", "Ready").into());
                        ui.add(egui::Label::new(RichText::new(last.trim()).size(12.0).color(log_color(&last))).truncate());
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let label = if self.log_open { tr!("Ocultar log", "Hide log") } else { "Log" };
                        if ui.add(egui::Button::new(RichText::new(label).size(12.0).color(p().dim)).frame(false)).clicked() {
                            self.log_open = !self.log_open;
                        }
                        if let Some(v) = &self.view {
                            let n: usize = v.devices.iter().map(|d| d.clips.len()).sum();
                            let txt = if v.synced {
                                let ok = v.devices.iter().flat_map(|d| &d.clips).filter(|c| c.status.placed()).count();
                                tr!("{} de {} clipes sincronizados pelo áudio", "{} of {} clips synced by audio", ok, n)
                            } else {
                                let cams = v.devices.iter().filter(|d| d.camera).count();
                                tr!("{} clipes · {} câmera(s) · {} gravador(es)", "{} clips · {} camera(s) · {} recorder(s)",
                                    n, cams, v.devices.len() - cams)
                            };
                            ui.label(RichText::new(txt).size(12.0).color(p().dim));
                            ui.add_space(10.0);
                        }
                    });
                });
            });
        if self.log_open {
            egui::TopBottomPanel::bottom("log").resizable(true).default_height(180.0).min_height(80.0).show_separator_line(false)
                .frame(egui::Frame::none().fill(p().bg).inner_margin(Margin::symmetric(12.0, 8.0)))
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                        for l in &self.log {
                            let col = if l.starts_with("---") { p().text } else { log_color(l) };
                            ui.label(RichText::new(l).monospace().size(11.5).color(col));
                        }
                    });
                });
        }
    }

    // ------------------------------------------------------------ inspector
    fn inspector(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("inspector").exact_width(290.0).resizable(false).show_separator_line(false)
            .frame(egui::Frame::none().fill(p().panel).inner_margin(Margin::symmetric(16.0, 14.0)))
            .show(ctx, |ui| {
                let Some(v) = self.view.clone().filter(|v| !v.devices.is_empty()) else {
                    ui.label(RichText::new(tr!("Nenhuma timeline carregada", "No timeline loaded")).color(p().faint));
                    return;
                };
                match self.sel {
                    Selection::Clip(d, c) if d < v.devices.len() && c < v.devices[d].clips.len() => self.clip_inspector(ui, &v, d, c),
                    Selection::Device(d) if d < v.devices.len() => self.device_inspector(ui, &v, d),
                    _ => {
                        ui.label(RichText::new(tr!("Clique num clipe ou numa faixa para ver os detalhes.",
                                                   "Click a clip or a track to see its details.")).color(p().faint));
                    }
                }
            });
    }

    fn kv(ui: &mut egui::Ui, k: &str, v: RichText) {
        ui.horizontal(|ui| {
            ui.add_sized([96.0, 18.0], egui::Label::new(RichText::new(k).color(p().faint).size(12.0)));
            ui.add(egui::Label::new(v.size(12.5)).wrap());
        });
    }

    fn clip_inspector(&mut self, ui: &mut egui::Ui, v: &View, di: usize, ci: usize) {
        let d = &v.devices[di];
        let c = &d.clips[ci];
        ui.horizontal(|ui| {
            let (r, resp) = ui.allocate_exact_size(Vec2::new(10.0, 18.0), Sense::click());
            ui.painter().rect_filled(Rect::from_center_size(r.center(), Vec2::new(4.0, 14.0)), 1.0, device_color(di));
            if ui.add(egui::Label::new(RichText::new(&d.name).color(p().dim)).sense(Sense::click())).clicked() || resp.clicked() {
                self.sel = Selection::Device(di);
            }
        });
        ui.add_space(2.0);
        ui.add(egui::Label::new(RichText::new(&c.name).family(semibold()).size(15.0).color(p().text)).wrap());
        ui.add(egui::Label::new(RichText::new(&c.path).size(11.0).color(p().faint)).wrap());
        ui.add_space(12.0);
        let st = if v.synced || c.status == Status::Offline {
            RichText::new(c.status.label()).color(status_color(c.status))
        } else {
            RichText::new(tr!("ainda não sincronizado", "not synced yet")).color(p().faint)
        };
        Self::kv(ui, tr!("Status", "Status"), st);
        Self::kv(ui, tr!("Posição", "Position"), RichText::new(tc(Some(c.start), v.fps)).monospace().color(p().text));
        if v.synced {
            Self::kv(ui, tr!("Deslocamento", "Offset"), RichText::new(fmt_delta(c.delta())).monospace().color(p().text));
        }
        Self::kv(ui, tr!("Duração", "Duration"), RichText::new(tc(Some(c.duration), v.fps)).monospace().color(p().dim));
        if c.score > 0.0 {
            ui.horizontal(|ui| {
                ui.add_sized([96.0, 18.0], egui::Label::new(RichText::new(tr!("Confiança", "Confidence")).color(p().faint).size(12.0)));
                let (r, _) = ui.allocate_exact_size(Vec2::new(90.0, 4.0), Sense::hover());
                ui.painter().rect_filled(r, 2.0, p().raised);
                let mut f = r;
                f.set_width(r.width() * (c.score / 40.0).clamp(0.05, 1.0) as f32);
                ui.painter().rect_filled(f, 2.0, p().synced);
                ui.label(RichText::new(format!("{:.1}", c.score)).monospace().size(12.0).color(p().dim));
            });
        }
        if !c.matched.is_empty() {
            Self::kv(ui, tr!("Casado com", "Matched to"), RichText::new(&c.matched).color(p().text));
        }
        let clock = match c.clock.as_str() {
            "sony-xml" => tr!("XML da Sony", "Sony XML"),
            "bwf" => "BWF (WAV)",
            "container" => tr!("metadado do arquivo", "file metadata"),
            "mtime" => tr!("data de modificação", "modification date"),
            _ => "—",
        };
        Self::kv(ui, tr!("Horário de", "Time from"), RichText::new(clock).color(p().dim));
        if v.synced && matches!(c.status, Status::ByTime | Status::Unsynced) {
            ui.add_space(10.0);
            let msg = if c.status == Status::ByTime {
                tr!("O áudio não casou. O clipe foi posicionado pelo horário de criação, a partir do vizinho sincronizado. Confira na timeline.",
                    "The audio did not match. The clip was placed by its creation time, relative to its synced neighbour. Check it on the timeline.")
            } else {
                tr!("Sem áudio casável nem horário confiável: o clipe ficou onde estava no XML.",
                    "No matching audio and no reliable creation time: the clip stays where it was in the XML.")
            };
            ui.add(egui::Label::new(RichText::new(msg).size(12.0).color(status_color(c.status))).wrap());
        }
    }

    fn device_inspector(&mut self, ui: &mut egui::Ui, v: &View, di: usize) {
        let d = &v.devices[di];
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::new(10.0, 20.0), Sense::hover());
            ui.painter().rect_filled(Rect::from_center_size(r.center(), Vec2::new(4.0, 16.0)), 1.0, device_color(di));
            ui.label(RichText::new(&d.name).family(semibold()).size(15.0).color(p().text));
        });
        let kind = if d.camera { tr!("câmera", "camera") } else { tr!("gravador", "recorder") };
        ui.label(RichText::new(tr!("{} · {} clipes", "{} · {} clips", kind, d.clips.len())).color(p().dim));
        ui.add_space(8.0);
        let is_ref = v.reference == Some(d.idx);
        let auto_ref = v.reference.is_none() && v.reference_device() == Some(d.idx);
        let label = if is_ref {
            tr!("Referência (fixada)", "Reference (pinned)")
        } else if auto_ref {
            tr!("Referência (automática)", "Reference (automatic)")
        } else {
            tr!("Usar como referência", "Use as reference")
        };
        if ui.add_enabled(!self.busy, egui::Button::new(label).selected(is_ref)).on_hover_text(tr!(
            "A referência é o dispositivo contra o qual os outros são alinhados. Por padrão, o gravador mais longo.",
            "The reference is the device the others are aligned to. By default, the longest recorder.")).clicked() {
            self.toggle_reference(d.idx);
        }
        ui.add_space(12.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (ci, c) in d.clips.iter().enumerate() {
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(rect, 3.0, p().raised);
                }
                let col = if v.synced || c.status == Status::Offline { status_color(c.status) } else { p().faint };
                ui.painter().circle_filled(Pos2::new(rect.left() + 6.0, rect.center().y), 3.0, col);
                ui.painter().text(Pos2::new(rect.left() + 16.0, rect.center().y), Align2::LEFT_CENTER, &c.name, ui_font(12.5), p().text);
                if v.synced {
                    ui.painter().text(Pos2::new(rect.right() - 4.0, rect.center().y), Align2::RIGHT_CENTER, fmt_delta(c.delta()),
                                      FontId::monospace(11.5), p().dim);
                }
                if resp.clicked() {
                    self.sel = Selection::Clip(di, ci);
                }
            }
        });
    }

    // ------------------------------------------------------------ timeline (main view)
    fn timeline_view(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().frame(egui::Frame::none().fill(p().bg)).show(ctx, |ui| {
            let Some(v) = self.view.clone().filter(|v| !v.devices.is_empty()) else {
                self.empty_state(ui, ctx);
                return;
            };
            self.timeline(ui, &v);
        });
    }

    fn empty_state(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let r = ui.max_rect();
        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            dashed_rect(ui.painter(), r.shrink(16.0), Stroke::new(1.5_f32, p().accent));
        }
        if self.busy {
            ui.put(Rect::from_center_size(r.center(), Vec2::splat(24.0)), egui::Spinner::new().size(20.0).color(p().accent));
            return;
        }
        let col = Rect::from_center_size(r.center(), Vec2::new(420.0, 300.0));
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(col), |ui| {
            ui.label(RichText::new(tr!("Importe a timeline", "Import the timeline")).family(semibold()).size(18.0).color(p().text));
            ui.add_space(4.0);
            ui.label(RichText::new(tr!(
                "XML Final Cut Pro 7 exportado do DaVinci Resolve ou do Premiere. Você também pode arrastar o arquivo para a janela.",
                "Final Cut Pro 7 XML exported from DaVinci Resolve or Premiere. You can also drop the file on the window.")).color(p().dim));
            ui.add_space(14.0);
            if ui.add(primary_button(tr!("Importar XML", "Import XML"))).clicked() {
                self.import_xml(ctx);
            }
            let recent: Vec<String> = self.store.recent.iter().filter(|x| Path::new(x).is_file()).cloned().collect();
            if !recent.is_empty() {
                ui.add_space(24.0);
                ui.label(RichText::new(tr!("Recentes", "Recent")).color(p().faint).size(12.0));
                ui.add_space(2.0);
                for path in recent {
                    let name = Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let dir = Path::new(&path).parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
                    let resp = ui.add(egui::Label::new(RichText::new(&name).color(p().text)).sense(Sense::click())).on_hover_text(&dir);
                    if resp.hovered() {
                        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if resp.clicked() {
                        self.open_path(ctx, &path);
                    }
                }
            }
        });
    }

    fn timeline(&mut self, ui: &mut egui::Ui, v: &View) {
        const HEAD_W: f32 = 200.0;
        const RULER_H: f32 = 26.0;
        let pal = p();
        let rect = ui.max_rect();
        let resp = ui.interact(rect, ui.id().with("timeline"), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let n = v.devices.len();
        let row_h = ((rect.height() - RULER_H) / n as f32).clamp(30.0, 120.0);
        let track = Rect::from_min_max(Pos2::new(rect.left() + HEAD_W, rect.top() + RULER_H), Pos2::new(rect.right() - 10.0, rect.bottom()));
        let total = (v.span * 1.02).max(1.0);
        let fit = track.width() as f64 / total;
        if v.synced {
            if let Some((view, sel)) = self.demo.take() {
                if let Some((a, b)) = view.filter(|(a, b)| b > a) {
                    self.px_per_sec = Some(track.width() as f64 / (b - a));
                    self.offset = a;
                }
                if let Some(d) = sel.filter(|&d| d < n) {
                    self.sel = Selection::Device(d);
                }
            }
        }
        let mut pps = self.px_per_sec.unwrap_or(fit).clamp(fit, 400.0);

        // ---- input: ctrl+wheel zooms around the pointer, wheel / drag pans
        if let Some(mp) = resp.hover_pos() {
            let (zoom, scroll) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta));
            if zoom != 1.0 && mp.x > track.left() {
                let tm = self.offset + (mp.x - track.left()) as f64 / pps;
                pps = (pps * zoom as f64).clamp(fit, 400.0);
                self.px_per_sec = Some(pps);
                self.offset = tm - (mp.x - track.left()) as f64 / pps;
            }
            let pan = if scroll.x.abs() > 0.0 { scroll.x } else { scroll.y };
            if pan != 0.0 && pps > fit * 1.001 {
                self.offset -= pan as f64 / pps;
            }
        }
        if resp.dragged() {
            self.offset -= resp.drag_delta().x as f64 / pps;
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        }
        self.offset = self.offset.clamp(0.0, (total - track.width() as f64 / pps).max(0.0));
        let off = self.offset;
        let x_of = |t: f64| track.left() + ((t - off) * pps) as f32;
        let lane_y = |di: usize| track.top() + di as f32 * row_h;
        let clip_rect = |di: usize, start: f64, dur: f64| {
            let y = lane_y(di);
            Rect::from_min_max(Pos2::new(x_of(start), y + 5.0), Pos2::new(x_of(start + dur).max(x_of(start) + 3.0), y + row_h - 5.0))
        };

        // ---- hit test
        let mut hovered: Option<(usize, usize)> = None;
        let mut hovered_head: Option<usize> = None;
        if let Some(h) = resp.hover_pos() {
            if h.x < track.left() && h.y > track.top() {
                let di = ((h.y - track.top()) / row_h) as usize;
                if di < n {
                    hovered_head = Some(di);
                }
            } else if track.contains(h) {
                for (di, d) in v.devices.iter().enumerate() {
                    for (ci, c) in d.clips.iter().enumerate() {
                        if clip_rect(di, c.start, c.duration).contains(h) {
                            hovered = Some((di, ci));
                        }
                    }
                }
            }
        }

        // ---- track headers
        let heads = Rect::from_min_max(Pos2::new(rect.left(), track.top()), Pos2::new(track.left(), rect.bottom()));
        painter.rect_filled(heads, 0.0, pal.panel);
        let ref_dev = v.reference_device();
        let mut ref_click: Option<usize> = None;
        for (di, d) in v.devices.iter().enumerate() {
            let y = lane_y(di);
            let lane = Rect::from_min_max(Pos2::new(rect.left(), y), Pos2::new(rect.right(), y + row_h));
            let head = Rect::from_min_max(lane.min, Pos2::new(track.left(), lane.bottom()));
            let selected = matches!(self.sel, Selection::Device(s) if s == di) || matches!(self.sel, Selection::Clip(s, _) if s == di);
            if selected {
                painter.rect_filled(head, 0.0, pal.raised);
            } else if hovered_head == Some(di) {
                painter.rect_filled(head, 0.0, pal.head_hover);
            }
            painter.rect_filled(Rect::from_min_size(head.min + Vec2::new(0.0, 1.0), Vec2::new(3.0, row_h - 2.0)), 0.0, device_color(di));
            painter.line_segment([Pos2::new(rect.left(), lane.bottom()), Pos2::new(rect.right(), lane.bottom())], Stroke::new(1.0_f32, pal.line));
            let hp = painter.with_clip_rect(head.shrink2(Vec2::new(0.0, 1.0)));
            let two = row_h >= 40.0;
            let ty = if two { head.center().y - 8.0 } else { head.center().y };
            hp.text(Pos2::new(head.left() + 14.0, ty), Align2::LEFT_CENTER, &d.name, ui_font(13.0), pal.text);
            if two {
                let kind = if d.camera { tr!("câmera", "camera") } else { tr!("gravador", "recorder") };
                hp.text(Pos2::new(head.left() + 14.0, head.center().y + 9.0), Align2::LEFT_CENTER,
                        tr!("{} · {} clipes", "{} · {} clips", kind, d.clips.len()), ui_font(11.5), pal.faint);
            }
            // result
            let (txt, col) = if !v.synced {
                (String::new(), pal.faint)
            } else if ref_dev == Some(d.idx) {
                ("ref.".to_string(), pal.reference)
            } else {
                let ok = d.clips.iter().filter(|c| c.status.placed()).count();
                let col = if ok == d.clips.len() { pal.synced } else if ok > 0 { pal.by_time } else { pal.unsynced };
                (format!("{ok}/{}", d.clips.len()), col)
            };
            hp.text(Pos2::new(head.right() - 34.0, ty), Align2::RIGHT_CENTER, txt, FontId::monospace(12.0), col);
            // reference toggle: a ring, filled when this device is the reference
            let rc = Pos2::new(head.right() - 16.0, ty);
            let ring = Rect::from_center_size(rc, Vec2::splat(16.0));
            let ring_hover = resp.hover_pos().is_some_and(|h| ring.contains(h));
            if ref_dev == Some(d.idx) {
                let c = if v.reference == Some(d.idx) { pal.reference } else { pal.dim };
                hp.circle_stroke(rc, 5.5, Stroke::new(1.5_f32, c));
                hp.circle_filled(rc, 2.5, c);
            } else if ring_hover || hovered_head == Some(di) {
                hp.circle_stroke(rc, 5.5, Stroke::new(1.2_f32, if ring_hover { pal.text } else { pal.faint }));
            }
            if ring_hover {
                let tip = if v.reference == Some(d.idx) {
                    tr!("Referência fixada (clique para voltar ao automático)", "Pinned reference (click to go back to automatic)")
                } else {
                    tr!("Usar como referência", "Use as reference")
                };
                egui::show_tooltip_at_pointer(ui.ctx(), ui.layer_id(), ui.id().with(("ref", di)), |ui| {
                    ui.label(tip);
                });
                if resp.clicked() {
                    ref_click = Some(d.idx);
                }
            }
        }
        // corner above the headers: fit / original toggles
        let corner = Rect::from_min_max(rect.min, Pos2::new(track.left(), track.top()));
        painter.rect_filled(corner, 0.0, pal.panel);
        let mut corner_ui = ui.new_child(egui::UiBuilder::new().max_rect(corner.shrink2(Vec2::new(8.0, 0.0)))
            .layout(Layout::left_to_right(Align::Center)));
        if corner_ui.add(egui::Button::new(RichText::new(tr!("Ajustar", "Fit")).size(12.0).color(pal.dim)).frame(false))
            .on_hover_text(tr!("Mostrar a timeline inteira (Ctrl + rolar dá zoom, arrastar move)",
                               "Show the whole timeline (Ctrl + scroll zooms, drag pans)")).clicked() {
            self.px_per_sec = None;
            self.offset = 0.0;
        }
        if v.synced {
            let lbl = RichText::new(tr!("Posição original", "Original position")).size(12.0)
                .color(if self.show_original { pal.text } else { pal.dim });
            if corner_ui.add(egui::Button::new(lbl).frame(false).selected(self.show_original))
                .on_hover_text(tr!("Mostrar onde cada clipe estava no XML importado", "Show where each clip was in the imported XML"))
                .clicked() {
                self.show_original = !self.show_original;
            }
        }

        // ---- ruler
        let ruler = Rect::from_min_max(Pos2::new(track.left(), rect.top()), Pos2::new(rect.right(), track.top()));
        painter.rect_filled(ruler, 0.0, pal.panel);
        painter.line_segment([Pos2::new(rect.left(), track.top()), Pos2::new(rect.right(), track.top())], Stroke::new(1.0_f32, pal.line));
        painter.line_segment([Pos2::new(track.left(), rect.top()), Pos2::new(track.left(), rect.bottom())], Stroke::new(1.0_f32, pal.line));
        let long = v.span >= 3600.0;
        let step = [1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0]
            .into_iter().find(|s| s * pps >= 90.0).unwrap_or(7200.0);
        let gp = painter.with_clip_rect(Rect::from_min_max(Pos2::new(track.left() + 1.0, rect.top()), rect.max));
        let mut t = (off / step).floor() * step;
        while x_of(t) < rect.right() {
            let x = x_of(t);
            gp.line_segment([Pos2::new(x, ruler.bottom() - 6.0), Pos2::new(x, ruler.bottom())], Stroke::new(1.0_f32, pal.faint));
            gp.line_segment([Pos2::new(x, track.top()), Pos2::new(x, rect.bottom())], Stroke::new(1.0_f32, pal.grid));
            gp.text(Pos2::new(x + 4.0, ruler.center().y - 2.0), Align2::LEFT_CENTER, ruler_label(t, long), FontId::monospace(10.5), pal.dim);
            t += step;
        }

        // ---- clips
        let tp = painter.with_clip_rect(Rect::from_min_max(Pos2::new(track.left() + 1.0, track.top()), rect.max));
        let dark = is_dark();
        for (di, d) in v.devices.iter().enumerate() {
            let base = device_color(di);
            if self.show_original && v.synced {
                for c in &d.clips {
                    dashed_rect(&tp, clip_rect(di, c.orig, c.duration), Stroke::new(1.0_f32, alpha(pal.text, 70)));
                }
            }
            for (ci, c) in d.clips.iter().enumerate() {
                let r = clip_rect(di, c.start, c.duration);
                if r.right() < track.left() || r.left() > rect.right() {
                    continue;
                }
                let cp = tp.with_clip_rect(r.intersect(tp.clip_rect()));
                let is_sel = self.sel == Selection::Clip(di, ci);
                let is_hover = hovered == Some((di, ci));
                let state = if v.synced { c.status } else if c.status == Status::Offline { Status::Offline } else { Status::Synced };
                // body: solid when placed by audio (more opaque = more confident), hatched when placed by
                // creation time, outline only when not synchronised
                let lift = if is_hover { 20 } else { 0 };
                let (lo, span_a) = if dark { (45u8, 45.0) } else { (55u8, 55.0) };
                match state {
                    Status::Reference | Status::Synced => {
                        let conf = if v.synced && state == Status::Synced {
                            ((c.score / 40.0).clamp(0.0, 1.0) * span_a) as u8
                        } else {
                            (span_a / 2.0) as u8
                        };
                        cp.rect_filled(r, 2.0, alpha(base, lo + conf + lift));
                    }
                    Status::ByTime => {
                        cp.rect_filled(r, 2.0, alpha(base, 22 + lift));
                        let mut x = r.left() - r.height();
                        while x < r.right() {
                            cp.line_segment([Pos2::new(x, r.bottom()), Pos2::new(x + r.height(), r.top())], Stroke::new(1.0_f32, alpha(base, 80)));
                            x += 7.0;
                        }
                    }
                    Status::Unsynced | Status::Offline => {
                        cp.rect_filled(r, 2.0, alpha(base, 10 + lift));
                    }
                }
                // waveform (only when the audio has been read)
                if let Some(pk) = c.peaks.as_ref().filter(|pk| c.peak_max > 0.0 && !pk.is_empty()) {
                    let top_pad = if r.height() > 34.0 { 15.0 } else { 3.0 };
                    let wave = Rect::from_min_max(Pos2::new(r.left(), r.top() + top_pad), Pos2::new(r.right(), r.bottom() - 3.0));
                    let (mid, half) = (wave.center().y, wave.height() / 2.0);
                    let shade = if dark {
                        rgb(base.r() / 2 + 120, base.g() / 2 + 120, base.b() / 2 + 120)
                    } else {
                        rgb(base.r() / 2, base.g() / 2, base.b() / 2)
                    };
                    let wcol = alpha(shade, if matches!(state, Status::Reference | Status::Synced) { 160 } else { 80 });
                    let mut x = wave.left().max(track.left());
                    let x_end = wave.right().min(rect.right());
                    while x < x_end {
                        let t0 = off + (x - track.left()) as f64 / pps - c.start;
                        let t1 = t0 + 2.0 / pps;
                        let i0 = ((t0 * PEAKS_PER_SEC as f64).max(0.0) as usize).min(pk.len() - 1);
                        let i1 = ((t1 * PEAKS_PER_SEC as f64).ceil().max(0.0) as usize).clamp(i0 + 1, pk.len());
                        let amp = pk[i0..i1].iter().cloned().fold(0f32, f32::max) / c.peak_max;
                        let h = (amp * half * 0.9).max(0.5);
                        cp.line_segment([Pos2::new(x, mid - h), Pos2::new(x, mid + h)], Stroke::new(1.0_f32, wcol));
                        x += 2.0;
                    }
                }
                // outline
                let outline = if is_sel { Stroke::new(1.5_f32, pal.text) } else { Stroke::new(1.0_f32, alpha(base, 200)) };
                match state {
                    Status::Unsynced | Status::Offline if !is_sel => dashed_rect(&cp, r.shrink(0.5), outline),
                    _ => {
                        cp.rect_stroke(r.shrink(0.5), 2.0, outline);
                    }
                }
                // label: name, and the applied offset once synced
                if r.height() > 34.0 && r.width() > 40.0 {
                    let lx = r.left().max(track.left()) + 5.0;
                    let name_r = cp.text(Pos2::new(lx, r.top() + 8.0), Align2::LEFT_CENTER, &c.name, ui_font(11.5), alpha(pal.text, 230));
                    if v.synced && c.status != Status::Reference && r.right() - name_r.right() > 60.0 {
                        cp.text(Pos2::new(name_r.right() + 8.0, r.top() + 8.0), Align2::LEFT_CENTER, fmt_delta(c.delta()),
                                FontId::monospace(10.5), alpha(pal.text, 150));
                    }
                }
            }
        }

        // ---- interaction results
        if resp.clicked() {
            if let Some(i) = ref_click {
                self.toggle_reference(i);
            } else if let Some((di, ci)) = hovered {
                self.sel = Selection::Clip(di, ci);
            } else if let Some(di) = hovered_head {
                self.sel = Selection::Device(di);
            } else {
                self.sel = Selection::None;
            }
        }
        if let Some((di, ci)) = hovered {
            let c = &v.devices[di].clips[ci];
            resp.on_hover_ui_at_pointer(|ui| {
                ui.label(RichText::new(&c.name).family(semibold()));
                let mut s = format!("{} · {}", v.devices[di].name, tc(Some(c.start), v.fps));
                if v.synced {
                    s += &format!(" · {}", c.status.label());
                    if c.status != Status::Reference {
                        s += &format!(" · {}", fmt_delta(c.delta()));
                    }
                }
                ui.label(RichText::new(s).color(p().dim));
            });
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
        let heading = |ui: &mut egui::Ui, t: &str| {
            ui.add_space(10.0);
            ui.label(RichText::new(t).family(semibold()).color(p().text));
            ui.add_space(2.0);
        };
        egui::Window::new(RichText::new(tr!("Configurações", "Settings")).family(semibold())).collapsible(false).resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO).frame(egui::Frame::window(&ctx.style()).inner_margin(Margin::same(18.0)))
            .show(ctx, |ui| {
                ui.set_width(460.0);
                heading(ui, tr!("Aparência", "Appearance"));
                egui::Grid::new("look").num_columns(2).spacing([14.0, 6.0]).show(ui, |ui| {
                    ui.label(RichText::new(tr!("Idioma", "Language")).color(p().dim));
                    let lang_label = |c: &str| match c {
                        "pt" => "Português",
                        "en" => "English",
                        _ => tr!("Automático (sistema)", "Automatic (system)"),
                    };
                    egui::ComboBox::from_id_salt("lang").selected_text(lang_label(&draft.s.language)).width(200.0).show_ui(ui, |ui| {
                        for c in ["en", "pt", "auto"] {
                            ui.selectable_value(&mut draft.s.language, c.to_string(), lang_label(c));
                        }
                    });
                    ui.end_row();
                    ui.label(RichText::new(tr!("Tema", "Theme")).color(p().dim));
                    let theme_label = |c: &str| match c {
                        "dark" => tr!("Escuro", "Dark"),
                        "light" => tr!("Claro", "Light"),
                        _ => tr!("Automático (sistema)", "Automatic (system)"),
                    };
                    egui::ComboBox::from_id_salt("theme").selected_text(theme_label(&draft.s.theme)).width(200.0).show_ui(ui, |ui| {
                        for c in ["system", "dark", "light"] {
                            ui.selectable_value(&mut draft.s.theme, c.to_string(), theme_label(c));
                        }
                    });
                    ui.end_row();
                });

                heading(ui, tr!("Sincronização", "Synchronisation"));
                egui::Grid::new("sync").num_columns(2).spacing([14.0, 6.0]).show(ui, |ui| {
                    ui.label(RichText::new(tr!("Ordem dos clipes", "Clip order")).color(p().dim));
                    egui::ComboBox::from_id_salt("order").selected_text(order_label(&draft.s.order)).width(200.0).show_ui(ui, |ui| {
                        for k in ["auto", "ctime", "xml", "name"] {
                            ui.selectable_value(&mut draft.s.order, k.to_string(), order_label(k));
                        }
                    });
                    ui.end_row();
                });
                ui.checkbox(&mut draft.s.chronology, tr!("Manter a ordem cronológica dos clipes de cada câmera",
                                                         "Keep each camera's clips in chronological order"));

                heading(ui, tr!("Exportação", "Export"));
                egui::Grid::new("export").num_columns(2).spacing([14.0, 6.0]).show(ui, |ui| {
                    for (label, val, id) in [(tr!("Label dos sincronizados", "Label for synced clips"), &mut draft.s.label_synced, "ls"),
                                             (tr!("Label dos não sincronizados", "Label for unsynced clips"), &mut draft.s.label_unsynced, "lu")] {
                        ui.label(RichText::new(label).color(p().dim));
                        let none = tr!("(nenhuma)", "(none)");
                        let shown = if val.is_empty() { none.to_string() } else { val.clone() };
                        egui::ComboBox::from_id_salt(id).selected_text(shown).width(200.0).show_ui(ui, |ui| {
                            ui.selectable_value(val, String::new(), none);
                            for c in LABEL_COLORS {
                                ui.selectable_value(val, c.to_string(), *c);
                            }
                        });
                        ui.end_row();
                    }
                    ui.label(RichText::new(tr!("Sufixo do arquivo", "File suffix")).color(p().dim));
                    ui.add(egui::TextEdit::singleline(&mut draft.s.suffix).desired_width(200.0));
                    ui.end_row();
                });

                heading(ui, tr!("Cache de áudio", "Audio cache"));
                ui.checkbox(&mut draft.s.use_cache, tr!("Guardar o áudio analisado (re-sincronizações ficam mais rápidas)",
                                                        "Keep the analysed audio (re-syncs get faster)"));
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut draft.cache_dir).desired_width(320.0));
                    if ui.button("…").on_hover_text(tr!("Escolher pasta", "Choose folder")).clicked() {
                        if let Some(x) = rfd::FileDialog::new().set_title(tr!("Pasta do cache", "Cache folder")).pick_folder() {
                            draft.cache_dir = x.to_string_lossy().into_owned();
                        }
                    }
                    if ui.button(tr!("Limpar", "Clear")).clicked() {
                        let ok = rfd::MessageDialog::new().set_title(APP)
                            .set_description(tr!("Limpar o cache deixa mais lenta a re-sincronização de arquivos já analisados. Continuar?",
                                                 "Clearing the cache makes re-syncing already analysed files slower. Continue?"))
                            .set_buttons(rfd::MessageButtons::YesNo).show();
                        if ok == rfd::MessageDialogResult::Yes {
                            AudioCache::new(Some(&draft.cache_dir), true).clear();
                        }
                    }
                });

                heading(ui, tr!("Mídia que mudou de lugar", "Media that moved"));
                ui.label(RichText::new(tr!("Redirecionar caminhos, um por linha (antigo => novo)", "Redirect paths, one per line (old => new)"))
                    .color(p().dim).size(12.0));
                ui.add(egui::TextEdit::multiline(&mut draft.maps).desired_rows(2).desired_width(f32::INFINITY)
                    .font(TextStyle::Monospace).hint_text("E:/Trabalhos => /Volumes/HD/Trabalhos"));
                ui.add_space(4.0);
                ui.label(RichText::new(tr!("Procurar arquivos ausentes nestas pastas, uma por linha", "Look for missing files in these folders, one per line"))
                    .color(p().dim).size(12.0));
                ui.add(egui::TextEdit::multiline(&mut draft.dirs).desired_rows(2).desired_width(f32::INFINITY).font(TextStyle::Monospace));
                if ui.add(egui::Button::new(RichText::new(tr!("Adicionar pasta…", "Add folder…")).color(p().accent)).frame(false)).clicked() {
                    if let Some(x) = rfd::FileDialog::new().set_title(tr!("Pasta com as mídias", "Media folder")).pick_folder() {
                        if !draft.dirs.trim().is_empty() {
                            draft.dirs.push('\n');
                        }
                        draft.dirs.push_str(&x.to_string_lossy());
                    }
                }
                ui.add_space(14.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(primary_button(tr!("Salvar", "Save"))).clicked() {
                        save = true;
                    }
                    if ui.add(tool_button(tr!("Cancelar", "Cancel"))).clicked() {
                        close = true;
                    }
                });
            });
        if save {
            let mut s = self.store.clone();
            s.language = draft.s.language.clone();
            s.theme = draft.s.theme.clone();
            s.order = draft.s.order.clone();
            s.chronology = draft.s.chronology;
            s.use_cache = draft.s.use_cache;
            s.label_synced = draft.s.label_synced.clone();
            s.label_unsynced = draft.s.label_unsynced.clone();
            s.suffix = draft.s.suffix.clone();
            s.cache_dir = if draft.cache_dir == default_cache_dir().to_string_lossy() { String::new() } else { draft.cache_dir.clone() };
            s.path_map = draft.maps.lines().filter_map(|l| l.split_once("=>"))
                .map(|(a, b)| (a.trim().to_string(), b.trim().to_string())).filter(|(a, b)| !a.is_empty() && !b.is_empty()).collect();
            s.search_dirs = draft.dirs.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect();
            sincra::i18n::set_lang(&s.language);
            ctx.set_theme(theme_pref(&s.theme));
            self.store = s;
            self.store.save();
            close = true;
        }
        if close {
            self.settings = None;
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        IS_DARK.store(ctx.theme() == egui::Theme::Dark, Ordering::Relaxed);
        self.poll();
        if let Some(path) = self.pending_open.take() {
            self.open_path(ctx, &path);
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        if let Some(path) = dropped.first() {
            self.open_path(ctx, &path.to_string_lossy());
        }
        if self.settings.is_none() && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.sel = Selection::None;
        }
        self.toolbar(ctx);
        self.status_bar(ctx);
        self.inspector(ctx);
        self.timeline_view(ctx);
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
            .with_inner_size([1360.0, 820.0])
            .with_min_inner_size([960.0, 560.0])
            .with_drag_and_drop(true)
            .with_icon(Arc::new(app_icon())),
        ..Default::default()
    };
    eframe::run_native(APP, options, Box::new(move |cc| Ok(Box::new(App::new(cc, open)))))
}
