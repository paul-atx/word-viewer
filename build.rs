//! Embeds assets/word-viewer.ico into the Windows exe.
//!
//! Uses the RC environment variable if set (any rc.exe-compatible compiler, e.g.
//! "zig rc"), otherwise rc.exe from the Windows SDK for MSVC builds, or windres
//! for GNU builds. Without one, the exe still builds, just without its icon.
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/word-viewer.rc");
    println!("cargo:rerun-if-changed=assets/word-viewer.ico");
    println!("cargo:rerun-if-env-changed=RC");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let rc = Path::new("assets/word-viewer.rc");
    let msvc = env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");

    let built = if let Ok(cmd) = env::var("RC") {
        let res = out.join("word-viewer.res");
        let mut parts = cmd.split_whitespace();
        let ok = parts.next().is_some_and(|exe| {
            Command::new(exe).args(parts).arg("/fo").arg(&res).arg(rc).status().is_ok_and(|s| s.success())
        });
        ok.then_some(res)
    } else if msvc {
        let res = out.join("word-viewer.res");
        sdk_rc().and_then(|exe| {
            let ok = Command::new(exe).args(["/nologo", "/fo"]).arg(&res).arg(rc).status().is_ok_and(|s| s.success());
            ok.then_some(res)
        })
    } else {
        let obj = out.join("word_viewer_res.o");
        ["x86_64-w64-mingw32-windres", "windres"].iter().find_map(|exe| {
            let ok = Command::new(exe).arg(rc).args(["-O", "coff", "-o"]).arg(&obj).status().is_ok_and(|s| s.success());
            ok.then(|| obj.clone())
        })
    };

    match built {
        Some(res) => println!("cargo:rustc-link-arg-bins={}", res.display()),
        None => println!("cargo:warning=no resource compiler found; building without the icon"),
    }
}

/// Newest x64 rc.exe from the Windows 10/11 SDK.
fn sdk_rc() -> Option<PathBuf> {
    let bin = Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let mut found: Vec<PathBuf> = std::fs::read_dir(bin)
        .ok()?
        .filter_map(|e| Some(e.ok()?.path().join(r"x64\rc.exe")))
        .filter(|p| p.is_file())
        .collect();
    found.sort();
    found.pop()
}
