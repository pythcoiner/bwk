//! Every shape the derive refuses must be a compile error, not a silent no-op.

use std::process::Command;

#[test]
fn rejected_shapes_fail_to_compile() {
    // The .stderr snapshots record stable's diagnostics, and nightly renders
    // some spans differently, so only stable checks them.
    if !on_stable() {
        return;
    }
    trybuild::TestCases::new().compile_fail("tests/ui/*.rs");
}

fn on_stable() -> bool {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let version = Command::new(rustc)
        .arg("--version")
        .output()
        .expect("rustc --version");
    let version = String::from_utf8_lossy(&version.stdout);
    !version.contains("nightly") && !version.contains("beta")
}
