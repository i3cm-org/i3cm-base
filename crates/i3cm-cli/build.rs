// Build stamp: the source revision goes into the program (`i3cm --version`, `GET /v1/version`).
// `I3CM_REV` (e.g. set by the image build) wins; otherwise git, re-run when HEAD moves.
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    Command::new("git").args(args).output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn main() {
    println!("cargo:rerun-if-env-changed=I3CM_REV");
    if let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        println!("cargo:rerun-if-changed={dir}/HEAD");
        println!("cargo:rerun-if-changed={dir}/packed-refs");
        if let Some(r) = git(&["symbolic-ref", "-q", "HEAD"]) {
            println!("cargo:rerun-if-changed={dir}/{r}");
        }
    }
    let rev = std::env::var("I3CM_REV").ok().filter(|s| !s.is_empty()).or_else(|| git(&["rev-parse", "--short=12", "HEAD"]));
    println!("cargo:rustc-env=I3CM_REV={}", rev.unwrap_or_else(|| "unknown".into()));
}
