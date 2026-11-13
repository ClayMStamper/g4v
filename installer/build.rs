// Locates the g4v release binary to embed, and the version to display.
use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("..");
    let payload = root.join("target").join("release").join("g4v.exe");
    if !payload.exists() {
        panic!("{} not found. Run `cargo build --release` in the repository root first (or use installer/build.ps1).", payload.display());
    }
    let payload = payload.canonicalize().unwrap();
    println!("cargo:rustc-env=G4V_PAYLOAD={}", payload.display());
    println!("cargo:rerun-if-changed={}", payload.display());

    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
    let version = manifest
        .lines()
        .find_map(|l| l.strip_prefix("version = ").map(|v| v.trim_matches('"').to_string()))
        .unwrap_or_else(|| "0.0.0".into());
    println!("cargo:rustc-env=G4V_VERSION={version}");
    println!("cargo:rerun-if-changed={}", root.join("Cargo.toml").display());

    // Give the setup program the g4v icon too.
    let ico = root.join("assets").join("g4v.ico").canonicalize().unwrap();
    println!("cargo:rerun-if-changed={}", ico.display());
    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico.to_str().unwrap());
    res.set("FileDescription", "g4v Setup");
    res.compile().expect("failed to embed the Windows icon resource");
}
