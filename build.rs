// Embeds the icon and version info in the Windows executables (sincra.exe and sincra-gui.exe).
fn main() {
    println!("cargo:rerun-if-changed=packaging/icon/sincra.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("packaging/icon/sincra.ico")
            .set("ProductName", "Sincra")
            .set("FileDescription", "Sincra - multicam sync by audio")
            .set("LegalCopyright", "MIT License");
        if let Err(e) = res.compile() {
            // a missing resource compiler must not break the build; the exe just has no icon
            println!("cargo:warning=could not embed the Windows icon: {e}");
        }
    }
}
