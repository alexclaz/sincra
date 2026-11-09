//! Command line interface. Without arguments it opens the graphical interface.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use clap::Parser;
use sincra::engine::{DEFAULT_MODE, MODES};
use sincra::project::{Options, Project};
use sincra::tr;

fn mode_names() -> Vec<&'static str> {
    MODES.iter().map(|m| m.name).collect()
}

#[derive(Parser, Debug)]
#[command(name = "sincra", version,
          about = tr!("Sincroniza automaticamente pelo áudio as câmeras e gravadores de uma timeline Final Cut Pro 7 XML (DaVinci Resolve / Premiere).",
                      "Automatically syncs the cameras and recorders of a Final Cut Pro 7 XML timeline (DaVinci Resolve / Premiere) by audio."))]
struct Args {
    #[arg(help = tr!("XML de entrada (FCP7 xmeml). Sem argumentos abre a interface gráfica.",
                     "Input XML (FCP7 xmeml). Without arguments the graphical interface opens."))]
    xml: Option<String>,
    #[arg(short, long, help = tr!("XML de saída (padrão: '<entrada> - synced.xml')", "Output XML (default: '<input> - synced.xml')"))]
    output: Option<PathBuf>,
    #[arg(short, long, default_value_t = 0, help = tr!("índice da sequência no XML", "sequence index in the XML"))]
    sequence: usize,
    #[arg(short, long, default_value = DEFAULT_MODE, value_parser = clap::builder::PossibleValuesParser::new(mode_names()),
          help = tr!("velocidade/precisão", "speed/precision"))]
    mode: String,
    #[arg(long, default_value = "auto", value_parser = ["auto", "ctime", "xml", "name"],
          help = tr!("ordem cronológica dos clipes de cada dispositivo (auto = decide pelo áudio)",
                     "chronological order of each device's clips (auto = decided from the audio)"))]
    order: String,
    #[arg(long, help = tr!("não forçar a ordem cronológica dos clipes", "do not enforce the chronological order of clips"))]
    no_chronology: bool,
    #[arg(long, help = tr!("índice do dispositivo de referência (veja --list)", "index of the reference device (see --list)"))]
    reference: Option<usize>,
    #[arg(long, help = tr!("cor de label dos clipes sincronizados", "label color for synced clips"))]
    label_synced: Option<String>,
    #[arg(long, default_value = "Rose", help = tr!("cor de label dos não sincronizados ('' desativa)", "label color for unsynced clips ('' disables)"))]
    label_unsynced: String,
    #[arg(long, default_value = " - synced", allow_hyphen_values = true,
          help = tr!("sufixo do arquivo e da sequência exportados", "suffix of the exported file and sequence"))]
    suffix: String,
    #[arg(long, help = tr!("não gravar cache de áudio", "do not write the audio cache"))]
    no_cache: bool,
    #[arg(long, help = tr!("pasta do cache de áudio", "audio cache folder"))]
    cache_dir: Option<String>,
    #[arg(long, value_name = tr!("ANTIGO=NOVO", "OLD=NEW"),
          help = tr!("redireciona um prefixo de caminho (ex.: E:/Trabalhos=/Volumes/HD/Trabalhos)",
                     "redirect a path prefix (e.g. E:/Projects=/Volumes/HD/Projects)"))]
    remap: Vec<String>,
    #[arg(long, value_name = tr!("PASTA", "FOLDER"),
          help = tr!("procura arquivos ausentes por nome nesta pasta (recursivo)", "look for missing files by name in this folder (recursive)"))]
    search: Vec<String>,
    #[arg(long, help = tr!("só lista sequências/dispositivos, sem sincronizar", "only list sequences/devices, do not sync"))]
    list: bool,
    #[arg(short, long, help = tr!("sem mensagens", "no messages"))]
    quiet: bool,
    #[arg(long, default_value = "auto", value_parser = ["auto", "pt", "en"],
          help = tr!("idioma das mensagens (auto = do sistema)", "message language (auto = system)"))]
    lang: String,
}

fn launch_gui() -> ExitCode {
    let exe = std::env::current_exe().ok().and_then(|e| e.canonicalize().ok().or(Some(e)));
    let name = if cfg!(windows) { "sincra-gui.exe" } else { "sincra-gui" };
    if let Some(gui) = exe.as_ref().and_then(|e| e.parent()).map(|d| d.join(name)).filter(|p| p.is_file()) {
        match std::process::Command::new(gui).spawn() {
            Ok(_) => return ExitCode::SUCCESS,
            Err(e) => eprintln!("{}", tr!("Erro ao abrir a interface: {}", "Could not open the interface: {}", e)),
        }
    } else {
        eprintln!("{}", tr!("Interface gráfica ({}) não encontrada ao lado deste executável. Use --help para a linha de comando.",
                            "Graphical interface ({}) not found next to this executable. See --help for the command line.", name));
    }
    ExitCode::FAILURE
}

fn main() -> ExitCode {
    // the language must be known before clap builds the help text
    let argv: Vec<String> = std::env::args().collect();
    let lang = argv.iter().position(|a| a == "--lang").and_then(|i| argv.get(i + 1).cloned())
        .or_else(|| argv.iter().find_map(|a| a.strip_prefix("--lang=").map(str::to_string)))
        .unwrap_or_else(|| "auto".into());
    sincra::i18n::set_lang(&lang);

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
            eprintln!("{} {e:#}", tr!("Erro:", "Error:"));
            ExitCode::FAILURE
        }
    }
}

fn run(args: &Args, xml: &str, opts: Options, log: sincra::engine::LogFn) -> anyhow::Result<()> {
    let mut proj = Project::new(xml, args.sequence, opts, log)?;
    if args.list {
        for (i, n) in proj.doc.sequence_names().iter().enumerate() {
            println!("{}", tr!("sequência {}: {}", "sequence {}: {}", i, n));
        }
    }
    proj.load(None)?;
    if args.list {
        for d in &proj.devices {
            println!("{}", tr!("dispositivo {}: {} [{}] {} clipes", "device {}: {} [{}] {} clips", d.idx, d.name, d.kind, d.clips.len()));
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
        println!("\n{}", tr!("Resultado:", "Result:"));
        for r in proj.summary() {
            let t = r.t.map(|t| format!("{t:.3}s")).unwrap_or_else(|| "-".into());
            println!("  {:<12} {:<32} {:<22} {:>12}  z={:.1}", r.device, r.clip, r.status.label(), t, r.score);
        }
        println!("\n{} {}", tr!("Salvo em:", "Saved to:"), out.display());
    }
    Ok(())
}
