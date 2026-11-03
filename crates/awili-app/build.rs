//! Targeting Windows only: give `awili-app.exe` its version resource (ProductName,
//! FileVersion, ProductVersion...) and the app icon, so File Explorer's
//! Properties and the taskbar show Awili. Every other target does nothing.
//!
//! The icon is built here from the brand PNGs (an `.ico` is a small directory of
//! PNG images), so no binary icon file has to be kept in the tree.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/brand/app-icons/generic-png");
    // `CARGO_CFG_TARGET_OS` is the TARGET; a `#[cfg(windows)]` here would test the
    // HOST (a cross build to Windows would lose the resources, and a Windows host
    // building another target would try to embed them).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        if let Err(e) = windows_resources() {
            // A missing resource compiler must not make the app unbuildable.
            println!("cargo:warning=Windows version resources skipped: {e}");
        }
    }
}

/// Sizes taken from `assets/brand/app-icons/generic-png/awili-<n>.png`.
const ICON_SIZES: [u32; 6] = [16, 32, 48, 64, 128, 256];

/// Write a multi-resolution `.ico` made of PNG payloads (supported since
/// Windows Vista). A size of 256 is stored as 0 in the directory.
fn write_ico(dir: &std::path::Path, out: &std::path::Path) -> std::io::Result<()> {
    let mut images: Vec<(u32, Vec<u8>)> = Vec::new();
    for size in ICON_SIZES {
        let png = std::fs::read(dir.join(format!("awili-{size}.png")))?;
        images.push((size, png));
    }
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&0u16.to_le_bytes()); // reserved
    bytes.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    bytes.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (size, png) in &images {
        let dim = if *size >= 256 { 0u8 } else { *size as u8 };
        bytes.push(dim); // width
        bytes.push(dim); // height
        bytes.push(0); // palette colours
        bytes.push(0); // reserved
        bytes.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        bytes.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        bytes.extend_from_slice(&(png.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in &images {
        bytes.extend_from_slice(png);
    }
    std::fs::write(out, bytes)
}

fn windows_resources() -> Result<(), Box<dyn std::error::Error>> {
    use std::path::PathBuf;
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let out = PathBuf::from(std::env::var("OUT_DIR")?);
    let ico = out.join("awili.ico");
    write_ico(
        &manifest.join("../../assets/brand/app-icons/generic-png"),
        &ico,
    )?;
    let version = std::env::var("CARGO_PKG_VERSION")?;
    let mut res = winresource::WindowsResource::new();
    res.set_icon(&ico.to_string_lossy())
        .set("ProductName", "Awili")
        .set("FileDescription", "Awili")
        .set("InternalName", "awili-app")
        .set("OriginalFilename", "awili-app.exe")
        .set("FileVersion", &version)
        .set("ProductVersion", &version)
        // Generic on purpose: no company, no named copyright holder.
        .set("CompanyName", "Awili")
        .set("LegalCopyright", "Copyright (c) Awili contributors");
    res.compile()?;
    Ok(())
}
