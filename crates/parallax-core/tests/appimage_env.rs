//! Inside an AppImage the app's own libraries come first on LD_LIBRARY_PATH.
//! System programs (git, gh) must not load them: they fail with "symbol lookup
//! error" or "error while loading shared libraries". One test per process, as
//! it sets the environment.
#![cfg(target_os = "linux")]

use std::process::Command;

#[test]
fn system_programs_do_not_load_the_appimages_libraries() {
    if Command::new("git").arg("--version").output().is_err() {
        return;
    }
    let appdir = tempfile::tempdir().unwrap();
    let lib = appdir.path().join("usr/lib");
    std::fs::create_dir_all(&lib).unwrap();
    // Broken stand-ins for libraries git links against.
    for name in ["libz.so.1", "libpcre2-8.so.0", "libc.so.6"] {
        std::fs::write(lib.join(name), b"not a library").unwrap();
    }
    let path = std::env::var("PATH").unwrap_or_default();
    // SAFETY: this test binary runs this one test, and nothing else reads the environment meanwhile.
    unsafe {
        std::env::set_var("APPIMAGE", appdir.path().join("App.AppImage"));
        std::env::set_var("APPDIR", appdir.path());
        std::env::set_var("LD_LIBRARY_PATH", &lib);
        std::env::set_var("PATH", format!("{}:{path}", appdir.path().join("usr/bin").display()));
    }
    // As the app's own environment would start it: git cannot load its libraries.
    let plain = Command::new("git").arg("--version").output().unwrap();
    assert!(!plain.status.success(), "the stand-in libraries should break git");
    let stderr = String::from_utf8_lossy(&plain.stderr);
    assert!(stderr.contains("error while loading shared libraries"), "{stderr}");

    let fixed = parallax_core::process::command("git").arg("--version").output().unwrap();
    assert!(fixed.status.success(), "{}", String::from_utf8_lossy(&fixed.stderr));
    assert!(String::from_utf8_lossy(&fixed.stdout).starts_with("git version"));
}
