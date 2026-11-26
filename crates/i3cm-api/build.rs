// Build stamp: the source revision goes into the program (`i3cm --version`).
fn main() {
    let rev = std::env::var("I3CM_REV").ok().filter(|s| !s.is_empty()).or_else(|| {
        std::process::Command::new("git").args(["rev-parse", "--short=12", "HEAD"]).output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    });
    println!("cargo:rustc-env=I3CM_REV={}", rev.unwrap_or_else(|| "unknown".into()));
    println!("cargo:rerun-if-env-changed=I3CM_REV");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
}
