//! Capture the build target triple so the provider installer can select the
//! matching archive from the registry index at runtime.

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    println!("cargo:rustc-env=DOTENV_CLOUD_TARGET={target}");
    println!("cargo:rerun-if-changed=build.rs");
}
