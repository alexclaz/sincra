//! Interface language (Portuguese / English). Chosen from the system locale, or explicitly.

use std::sync::atomic::{AtomicU8, Ordering};

const UNSET: u8 = 0;
const PT: u8 = 1;
const EN: u8 = 2;

static LANG: AtomicU8 = AtomicU8::new(UNSET);

/// True when the system language is Portuguese (pt-BR, pt-PT...). $SINCRA_LANG overrides it.
pub fn system_is_pt() -> bool {
    if let Some(l) = std::env::var_os("SINCRA_LANG") {
        return l.to_string_lossy().to_lowercase().starts_with("pt");
    }
    // Portuguese in any preferred language, or in the regional format (a Brazilian with Windows/macOS in
    // English usually still wants Portuguese)
    let pt = |l: &str| l.to_lowercase().starts_with("pt");
    sys_locale::get_locales().any(|l| pt(&l)) || region_locale().is_some_and(|l| pt(&l))
}

#[cfg(windows)]
fn region_locale() -> Option<String> {
    use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;
    let mut buf = [0u16; 85];
    let n = unsafe { GetUserDefaultLocaleName(buf.as_mut_ptr(), buf.len() as i32) };
    (n > 1).then(|| String::from_utf16_lossy(&buf[..n as usize - 1]))
}

#[cfg(target_os = "macos")]
fn region_locale() -> Option<String> {
    // e.g. "pt_BR" or "en_BR"
    let o = std::process::Command::new("defaults").args(["read", "-g", "AppleLocale"]).output().ok()?;
    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn region_locale() -> Option<String> {
    std::env::var("LC_ALL").or_else(|_| std::env::var("LC_MESSAGES")).or_else(|_| std::env::var("LANG")).ok()
}

/// "pt", "en" or "auto" (system language).
pub fn set_lang(code: &str) {
    let pt = match code {
        "pt" => true,
        "en" => false,
        _ => system_is_pt(),
    };
    LANG.store(if pt { PT } else { EN }, Ordering::Relaxed);
}

pub fn is_en() -> bool {
    match LANG.load(Ordering::Relaxed) {
        UNSET => {
            set_lang("auto");
            is_en()
        }
        v => v == EN,
    }
}

/// `tr!("pt", "en")` picks a literal; `tr!("pt {}", "en {}", args...)` formats both with the same arguments.
#[macro_export]
macro_rules! tr {
    ($pt:literal, $en:literal) => {
        if $crate::i18n::is_en() { $en } else { $pt }
    };
    ($pt:literal, $en:literal, $($arg:expr),+ $(,)?) => {
        if $crate::i18n::is_en() { format!($en, $($arg),+) } else { format!($pt, $($arg),+) }
    };
}
