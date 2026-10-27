// Build script for the platform crate.
//
// Carbon (HIToolbox) provides RegisterEventHotKey for system-wide shortcuts.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-lib=framework=Carbon");
    }
}
