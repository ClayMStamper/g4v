// Embeds the application icon (and version info) into the Windows executable. Windows uses this icon for the
// taskbar, pinned items, shortcuts and Explorer, so it must live in the .exe itself, not just in the window.
fn main() {
    println!("cargo:rerun-if-changed=assets/g4v.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/g4v.ico");
        res.set("FileDescription", "g4v");
        res.set("ProductName", "g4v");
        res.compile().expect("failed to embed the Windows icon resource");
    }
}
