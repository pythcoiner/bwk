// SPDX-License-Identifier: CC0-1.0

//! Build script for bwk-spscan-sys.
//!
//! cc-compiles the relocated/symbol-renamed libsecp256k1 (prefix
//! `bwkspscan_v0_1_0_`) plus the byte-FFI shim. Mirrors secp256k1-sys/build.rs:
//! same ENABLE_MODULE_* defines and field/ecmult window config. No module
//! stripping in this step.

extern crate cc;

use std::env;

fn main() {
    // cc only tracks the explicit `.file()` entries, not the headers they
    // `#include` (the per-module `main_impl.h`, the shim's secp headers). Without
    // this a header edit leaves a stale object and the link sees outdated symbols.
    println!("cargo:rerun-if-changed=depend");
    println!("cargo:rerun-if-changed=build.rs");

    let mut base_config = cc::Build::new();
    base_config
        .include("depend/secp256k1/")
        .include("depend/secp256k1/include")
        .include("depend/secp256k1/src")
        .include("depend/secp256k1/src/modules/silentpayments")
        .flag_if_supported("-Wno-unused-function")
        .flag_if_supported("-Wno-unused-parameter")
        .define("SECP256K1_API", Some(""))
        // Stripped build: only extrakeys (x-only save/load/cmp/serialize used by
        // the SP light-client path + shim) and silentpayments are enabled. The
        // ecdh/schnorrsig/ellswift/recovery/musig modules are not referenced by
        // the two scan kernels and have been removed from depend/. See
        // depend/README.
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

    // libsecp + the byte-FFI shim. lax_der_parsing is a DER helper used only by
    // upstream tests (not the scan path), so it is not compiled here.
    base_config
        .file("depend/secp256k1/src/precomputed_ecmult_gen.c")
        .file("depend/secp256k1/src/precomputed_ecmult.c")
        .file("depend/secp256k1/src/secp256k1.c")
        .file("depend/secp256k1/src/modules/silentpayments/spscan_ffi.c");

    if base_config.try_compile("libbwkspscan.a").is_err() {
        base_config.include("wasm/wasm-sysroot");
        base_config.compile("libbwkspscan.a");
    }
}
