// SPDX-License-Identifier: CC0-1.0

//! Build script for secp256k1_spscan-sys.
//!
//! cc-compiles the libsecp256k1 SP-scan fork plus the byte-FFI shim.

extern crate cc;

use std::{env, path::Path, process::Command};

const SECP256K1_REQUIRED_FILES: &[&str] = &[
    "depend/secp256k1/include/secp256k1.h",
    "depend/secp256k1/include/secp256k1_silentpayments.h",
    "depend/secp256k1/src/precomputed_ecmult_gen.c",
    "depend/secp256k1/src/precomputed_ecmult.c",
    "depend/secp256k1/src/secp256k1.c",
];

fn main() {
    // cc only tracks the explicit `.file()` entries, not the headers they
    // `#include` (the per-module `main_impl.h`, the shim's secp headers). Without
    // this a header edit leaves a stale object and the link sees outdated symbols.
    println!("cargo:rerun-if-changed=depend");
    println!("cargo:rerun-if-changed=spscan_ffi.c");
    println!("cargo:rerun-if-changed=build.rs");

    init_secp256k1_submodule();

    let mut base_config = cc::Build::new();
    base_config
        .include("depend/secp256k1/")
        .include("depend/secp256k1/include")
        .include("depend/secp256k1/src")
        .include("depend/secp256k1/src/modules/silentpayments")
        .flag_if_supported("-Wno-unused-function")
        .flag_if_supported("-Wno-unused-parameter")
        .define("SECP256K1_API", Some("extern"))
        // Only extrakeys and silentpayments are needed by the SP scan path.
        .define("ENABLE_MODULE_EXTRAKEYS", Some("1"))
        .define("ENABLE_MODULE_SILENTPAYMENTS", Some("1"));

    base_config.define("ECMULT_GEN_PREC_BITS", Some("4"));
    base_config.define("ECMULT_WINDOW_SIZE", Some("15"));
    base_config.define("USE_EXTERNAL_DEFAULT_CALLBACKS", Some("1"));

    if env::var("CARGO_CFG_TARGET_ARCH").unwrap() == "wasm32" {
        // No real libc on wasm: strip printf (the freestanding sysroot lacks it).
        // Only here, since on native targets USE_EXTERNAL_DEFAULT_CALLBACKS drops
        // the only printf users, and `-Dprintf(...)=` mangles fortified glibc's
        // inline `printf` in <stdio.h> (breaks the build on hardened distros).
        base_config
            .define("printf(...)", Some(""))
            .include("wasm/wasm-sysroot")
            .file("wasm/wasm.c");
    }

    // libsecp + the byte-FFI shim.
    base_config
        .file("depend/secp256k1/src/precomputed_ecmult_gen.c")
        .file("depend/secp256k1/src/precomputed_ecmult.c")
        .file("depend/secp256k1/src/secp256k1.c")
        .file("spscan_ffi.c");

    if base_config.try_compile("libsecp256k1_spscan.a").is_err() {
        base_config.include("wasm/wasm-sysroot");
        base_config.compile("libsecp256k1_spscan.a");
    }
}

fn init_secp256k1_submodule() {
    if SECP256K1_REQUIRED_FILES
        .iter()
        .all(|path| Path::new(path).exists())
    {
        return;
    }

    let status = Command::new("git")
        .args([
            "submodule",
            "update",
            "--init",
            "--recursive",
            "depend/secp256k1",
        ])
        .status()
        .expect("failed to run git submodule update for secp256k1_spscan");
    assert!(
        status.success(),
        "failed to initialize secp256k1_spscan submodule"
    );

    for path in SECP256K1_REQUIRED_FILES {
        assert!(
            Path::new(path).exists(),
            "missing secp256k1_spscan submodule file `{path}` after initialization"
        );
    }
}
