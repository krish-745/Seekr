//! Fetches the pinned pdfium build (bblanchon/pdfium-binaries) once into target\pdfium, checks
//! it against its SHA-256, and copies pdfium.dll next to the binaries and tests, as ort does with
//! DirectML.dll. Windows' own curl.exe and tar.exe do the work, so nothing else is needed.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use sha2::{Digest, Sha256};

const BUILD: &str = "7881";
const SHA256: &str = "73cc0de638ac2095e7445bf56a38200a5b7c7ca0e9f4ba144598f2457377ac08";

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    // OUT_DIR is <target>\<profile>\build\content-<hash>\out.
    let profile = out.ancestors().nth(3).expect("the profile folder");
    let cache = profile
        .parent()
        .expect("the target folder")
        .join("pdfium")
        .join(BUILD);
    let dll = cache.join("bin").join("pdfium.dll");
    if !dll.exists() {
        fetch(&cache);
    }
    for dir in [profile.to_path_buf(), profile.join("deps")] {
        fs::create_dir_all(&dir).expect("the binaries' folder");
        fs::copy(&dll, dir.join("pdfium.dll")).expect("copying pdfium.dll");
    }
}

fn fetch(cache: &Path) {
    fs::create_dir_all(cache).expect("target\\pdfium");
    let tgz = cache.join("pdfium-win-x64.tgz");
    let url = format!(
        "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F{BUILD}/pdfium-win-x64.tgz"
    );
    // System32's copies: Git's tar reads C:\ as a remote host.
    let system = PathBuf::from(env::var_os("SystemRoot").expect("SystemRoot")).join("System32");
    run(Command::new(system.join("curl.exe"))
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--output",
        ])
        .arg(&tgz)
        .arg(&url));
    let digest = Sha256::digest(fs::read(&tgz).expect("the pdfium download"));
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    if hex != SHA256 {
        fs::remove_file(&tgz).ok();
        panic!("{url} has SHA-256 {hex}, not the pinned {SHA256}");
    }
    run(Command::new(system.join("tar.exe"))
        .arg("-xzf")
        .arg(&tgz)
        .arg("-C")
        .arg(cache)
        .args(["bin/pdfium.dll", "LICENSE", "licenses"]));
    fs::remove_file(&tgz).expect("removing the pdfium download");
}

fn run(command: &mut Command) {
    let status = command.status().expect("starting a download step");
    assert!(status.success(), "{command:?} failed with {status}");
}
