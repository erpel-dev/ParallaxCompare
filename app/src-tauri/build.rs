fn main() {
    // Source archives still build; a checkout also records its exact revision.
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|r| r.status.success())
            .and_then(|r| String::from_utf8(r.stdout).ok())
            .map(|s| s.trim().to_string())
    };
    let revision = git(&["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=PARALLAX_BUILD_REVISION={revision}");
    for name in [Some("HEAD".into()), git(&["symbolic-ref", "-q", "HEAD"])].into_iter().flatten() {
        if let Some(path) = git(&["rev-parse", "--git-path", &name]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    tauri_build::build()
}
