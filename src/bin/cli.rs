//! Command line interface. Without arguments it opens the graphical interface.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use clap::Parser;
use sincra::engine::{MODES, DEFAULT_MODE};
use sincra::project::{Options, Project};

fn mode_names() -> Vec<&'static str> {
    MODES.iter().map(|m| m.name).collect()
}

#[derive(Parser, Debug)]
#[command(name = "sincra", version,
          about = "Sincroniza automaticamente cameras e gravadores de uma timeline Final Cut Pro 7 XML (Premiere / DaVinci Resolve) pelo audio.")]
struct Args {
    /// XML de entrada (FCP7 xmeml). Sem argumentos abre a interface grafica.
    xml: Option<String>,
    /// XML de saida (padrao: '<entrada> - synced.xml')
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// indice da sequencia no XML
    #[arg(short, long, default_value_t = 0)]
    sequence: usize,
    /// velocidade/qualidade
    #[arg(short, long, default_value = DEFAULT_MODE, value_parser = clap::builder::PossibleValuesParser::new(mode_names()))]
    mode: String,
    /// ordem cronologica dos clipes de cada dispositivo (auto = decide pelo audio)
    #[arg(long, default_value = "auto", value_parser = ["auto", "ctime", "xml", "name"])]
    order: String,
    /// nao forcar a ordem cronologica dos clipes
    #[arg(long)]
    no_chronology: bool,
    /// indice do dispositivo de referencia (ver --list)
    #[arg(long)]
    reference: Option<usize>,
    /// cor de label para clipes sincronizados (Premiere)
    #[arg(long)]
    label_synced: Option<String>,
    /// cor de label para nao sincronizados ('' desativa)
    #[arg(long, default_value = "Rose")]
    label_unsynced: String,
    #[arg(long, default_value = " - synced", allow_hyphen_values = true)]
    suffix: String,
    /// nao gravar cache de audio
    #[arg(long)]
    no_cache: bool,
    #[arg(long)]
    cache_dir: Option<String>,
    /// redireciona prefixo de caminho (ex.: E:/Trabalhos=/Volumes/HD/Trabalhos)
    #[arg(long, value_name = "ANTIGO=NOVO")]
    remap: Vec<String>,
    /// procura arquivos ausentes por nome nesta pasta (recursivo)
    #[arg(long, value_name = "PASTA")]
    search: Vec<String>,
    /// so lista sequencias/dispositivos, sem sincronizar
    #[arg(long)]
    list: bool,
    #[arg(short, long)]
    quiet: bool,
}

fn launch_gui() -> ExitCode {
    let exe = std::env::current_exe().ok().and_then(|e| e.canonicalize().ok().or(Some(e)));
    let name = if cfg!(windows) { "sincra-gui.exe" } else { "sincra-gui" };
    if let Some(gui) = exe.as_ref().and_then(|e| e.parent()).map(|d| d.join(name)).filter(|p| p.is_file()) {
        match std::process::Command::new(gui).spawn() {
            Ok(_) => return ExitCode::SUCCESS,
            Err(e) => eprintln!("Erro ao abrir a interface: {e}"),
        }
    } else {
        eprintln!("Interface grafica ({name}) nao encontrada ao lado deste executavel. Use --help para a linha de comando.");
    }
    ExitCode::FAILURE
}

fn main() -> ExitCode {
    let args = Args::parse();
    let Some(xml) = args.xml.clone() else { return launch_gui() };
    let opts = Options {
        mode: args.mode.clone(),
        order: args.order.clone(),
        chronology: !args.no_chronology,
        reference: args.reference,
        label_synced: args.label_synced.clone().filter(|s| !s.is_empty()),
        label_unsynced: Some(args.label_unsynced.clone()).filter(|s| !s.is_empty()),
        suffix: args.suffix.clone(),
        use_cache: !args.no_cache,
        cache_dir: args.cache_dir.clone(),
        path_map: args.remap.iter().filter_map(|r| r.split_once('=').map(|(a, b)| (a.to_string(), b.to_string()))).collect(),
        search_dirs: args.search.clone(),
    };
    let quiet = args.quiet;
    let log: sincra::engine::LogFn = Arc::new(move |s: &str| {
        if !quiet {
            println!("{s}");
        }
    });
    match run(&args, &xml, opts, log) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Erro: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &Args, xml: &str, opts: Options, log: sincra::engine::LogFn) -> anyhow::Result<()> {
    let mut proj = Project::new(xml, args.sequence, opts, log)?;
    if args.list {
        for (i, n) in proj.doc.sequence_names().iter().enumerate() {
            println!("sequencia {i}: {n}");
        }
    }
    proj.load(None)?;
    if args.list {
        for d in &proj.devices {
            println!("dispositivo {}: {} [{}] {} clipes", d.idx, d.name, d.kind, d.clips.len());
        }
        return Ok(());
    }
    let last = Arc::new(Mutex::new((String::new(), -1i64)));
    let quiet = args.quiet;
    let progress: sincra::engine::ProgressFn = Arc::new(move |stage: &str, frac: f64| {
        let pct = (frac * 100.0) as i64;
        let mut l = last.lock().unwrap();
        if !quiet && (pct != l.1 || stage != l.0) && pct % 10 == 0 {
            *l = (stage.to_string(), pct);
            eprintln!("[{stage}] {pct}%");
        }
    });
    proj.synchronize(Some(progress), None)?;
    let out = proj.export(args.output.as_deref())?;
    if !args.quiet {
        println!("\nResultado:");
        for r in proj.summary() {
            let t = r.t.map(|t| format!("{t:.3}s")).unwrap_or_else(|| "-".into());
            println!("  {:<12} {:<32} {:<17} {:>12}  z={:.1}", r.device, r.clip, r.status.pt(), t, r.score);
        }
        println!("\nSalvo em: {}", out.display());
    }
    Ok(())
}
