//! Capture the short git commit hash at build time. Exposed to the program
//! via `option_env!("AKASHIC_BUILD_COMMIT")`. Used to populate the
//! `akashic_build_info{commit}` gauge in C5.
//!
//! Resolution order (C4):
//!   1. Pre-set `AKASHIC_BUILD_COMMIT` env var (e.g. from `docker build
//!      --build-arg AKASHIC_BUILD_COMMIT=...`). Wins so docker builds can
//!      embed the commit without needing `.git` in the build context.
//!   2. `git rev-parse --short=12 HEAD` from the workspace cwd.
//!   3. Fallback string `"unknown"`.

fn main() {
    let commit = std::env::var("AKASHIC_BUILD_COMMIT")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::process::Command::new("git")
                .args(["rev-parse", "--short=12", "HEAD"])
                .output()
                .ok()
                .and_then(|o| {
                    if o.status.success() {
                        String::from_utf8(o.stdout).ok()
                    } else {
                        None
                    }
                })
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=AKASHIC_BUILD_COMMIT={commit}");
    println!("cargo:rerun-if-changed=../../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../../.git/refs");
    println!("cargo:rerun-if-env-changed=AKASHIC_BUILD_COMMIT");
}
