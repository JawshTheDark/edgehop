//! Windows: build the app icon (and version details) into edgehop.exe, so
//! Explorer, the taskbar, shortcuts and Apps & Features all show it. The icon
//! is drawn by the same code the app uses for its window and tray icons.
#[allow(dead_code)]
#[path = "src/icon.rs"]
mod icon;

fn main() {
    println!("cargo:rerun-if-changed=src/icon.rs");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("edgehop.ico");
    std::fs::write(&out, icon::ico()).expect("write edgehop.ico");
    let mut res = winresource::WindowsResource::new();
    res.set_icon(out.to_str().expect("utf-8 path"));
    res.set("FileDescription", "Edgehop: keyboard and mouse sharing");
    res.set("ProductName", "Edgehop");
    res.set("CompanyName", "Edgehop");
    res.set("LegalCopyright", "MIT License");
    if let Err(e) = res.compile() {
        panic!("couldn't build the icon into edgehop.exe: {e}");
    }
}
