# bwk-qr: roadmap & progress checklist

Single source of truth for progress. Check items off (`[x]`) as they land. Each
phase has explicit acceptance criteria; do not mark a phase done until every
criterion passes. See `PLAN.md` for the design these items implement.

There are two goals: (1) provide QR generation + scanning via pure-Rust crates;
(2) implement the miniscript QR signing-flow protocol with our own hand-rolled,
versioned binary encoding, framed over BBQR. Phases below build both, interleaved
with Silent integration. The codec lives in `bwk-qr-protocol` so a bare-metal signer
can take it without the QR layer; `bwk-qr` is the QR and framing layer on top.

Legend: `[ ]` todo, `[~]` in progress, `[x]` done. Update this file in the same
commit as the work it describes.

---

## Phase 0 - Docs + empty crate skeleton (compiles, no logic)

### Docs
- [x] Write `qr/PLAN.md` (design)
- [x] Write `qr/ROADMAP.md` (this checklist)
- [x] Write `qr-protocol/ENCODING.md` (authoritative, transport-agnostic encoding)
- [x] Have the docs reviewed by pythcoiner before any code

### Crate skeleton (only after docs are approved)
- [x] Add `"qr"` and `"qr-protocol"` to `members` in `bwk/Cargo.toml`
- [x] `qr/Cargo.toml`: package `bwk-qr`; features `gen`, `scan`, `protocol`
  (implies `gen`+`scan`), all on by default; optional deps `qrcodegen` (behind `gen`),
  `quircs` (behind `scan`),
  `bwk-qr-protocol` (behind `protocol`); no serde/CBOR, no build script
- [x] `qr-protocol/Cargo.toml`: package `bwk-qr-protocol`; `no_std` + `alloc`, no
  dependency by default; features `bitcoin` (type adapters) and `ffi` (C binding)
- [x] `qr/src/lib.rs`: module decls (`config`, `error`, `image`, `gen`, `scan`,
  `encoder`, `decoder`, `bbqr`, `protocol`) feature-gated; crate docs stating the
  two goals
- [x] `qr/src/error.rs`: `Error` enum + `Display`/`std::error::Error`
- [x] `qr/src/{gen,scan}.rs` (internal helpers), `qr/src/{encoder,decoder,bbqr}.rs`
  (Encoder/Decoder and the BBQR framing) and
  `qr-protocol/src/{lib,types,request,response,reader,encode,decode}.rs`
  (the codec)

### Acceptance (Phase 0)
- [x] `cargo check -p bwk-qr` clean across: default, `--features scan`,
  `--features protocol`, `--all-features`
- [x] `cargo clippy -p bwk-qr --all-features` clean; `cargo fmt --check` clean
- [x] No Silent repo changes

---

## Phase 1 - Generation (feature `gen`) + Silent Receive QR

### bwk-qr
- [x] Add `qrcodegen` (feature `gen`) to `qr/Cargo.toml`
- [x] `gen.rs` (internal): `CorrectionLevel`, `Image`, `encode -> Result<Image>`;
  rasterize the qrcodegen modules (`get_module`) into a grayscale `Image`; `None` if
  too long
- [x] Public plain-QR path `Encoder::encode_text(&str) -> Result<Image>` (in
  `encoder.rs`): a single plain frame for anything that fits one QR (addresses
  always do), BBQR-animated only when too large for one code
- [x] `tests/gen.rs`: known-vector sizes; long SP-address payload within
  `max_qr_version`
- [x] `cargo test -p bwk-qr --features gen` green; clippy/fmt clean

### Silent (separate repo; bumps bwk rev)
- [ ] Add `bwk-qr` (features `gen`) to `silent/Cargo.toml`; bump `bwk` git `rev`
- [ ] Bridge: `struct Image { data, width, height }` +
  `fn qr_encode(data: &str) -> Image` via `Encoder::encode_text` (empty `data` =
  failure) in `lib.rs` + `account.rs`
- [ ] `./build.sh` regenerates `lib/include/silent.h`
- [ ] Rewrite `src/catalog/panels/receive/QrCode.cpp::paintEvent` to blit the raster
  into a cached `QImage` (integer scale, quiet zone, blank fallback) instead of
  drawing modules
- [ ] Refresh `Cargo.lock`, `flake.lock`, `flake.nix` `outputHashes`

### Acceptance (Phase 1)
- [x] `cargo test -p bwk-qr` green; clippy/fmt clean
- [ ] `just br`: Receive shows real QR for SP / segwit / taproot addresses
- [ ] Phone wallet scan decodes to exactly the displayed address
- [ ] Address-history modal QRs render; empty state still blank
- [ ] Crisp at 256px (modal) and `Size::M` (panel)

---

## Phase 2 - Signing-flow protocol core (feature `protocol`)

Pure bwk-qr; no camera, no Silent UI. The heart of goal 2.

- [x] Implement the generic-binary BBQR subset using the `B` file type
- [x] `qr-protocol/src/lib.rs`: `Request`/`Response` (request id + body enum) and the
  `Message` the decoder returns, with the payload structs for Get Xpubs, Register
  Descriptor, Address Verification, Signing split across `request.rs` and
  `response.rs` (per PLAN 4.1); plain types (no serde); byte-level leaf types in
  `types.rs` (`Xpub`, `Fingerprint`, `PublicKey`, `DerivationPath`, and the PSBT as an
  opaque `Vec<u8>`) so the crate needs no `bitcoin` dependency, with `From`/`TryFrom`
  adapters behind the `bitcoin` feature;
  descriptors as `DescriptorBody` (BIP-380 string or BIP-388 keys+policy);
  a silent-payment send is answered with the full PSBT, which already carries its
  BIP-375 fields
- [x] `qr-protocol/src/{reader,encode,decode}.rs`: binary encode/decode implementing
  `ENCODING.md` exactly; varint (CompactSize) + primitive helpers; envelope magic +
  `version` + `msg_type` + `request_id`; append-only field ordering, with the `since`
  convention stated once at the top of `lib.rs` (every field there is v1)
- [x] Strings are nul-free UTF-8 on both sides, so a nul-terminated consumer cannot
  truncate one; a vector `COUNT` never reserves before its items are read
- [x] Validation: at most one `bip380`/`bip388` per Register request; signing
  response is exactly one of Psbt / Signatures; per-kind `SIG_KIND` mapping;
  reserved capability bits and colliding error codes rejected on encode;
  truncated/garbage -> `Error::Decode`
- [x] `Encoder` (`new`; `encode_text` for plain QRs;
  `encode_request`/`encode_response` -> `Vec<Image>`); `Config` with sane QR-density
  defaults
- [x] `Decoder` wrapping BBQR reassembly + `quircs` scan
  (`process(&Image)`; `progress`/`reset`, running the codec on joined data)
- [x] Map BBQR errors into `Error::Bbqr`
- [x] `tests/protocol.rs`: round-trip per message (`Encoder::encode_* -> render ->
  Decoder`); version-compat (a v1 parser accepts a newer version and ignores its
  trailing field, and still reads a plain v1 blob); ordered, shuffled and duplicate
  frames; two messages through one `Decoder`; malformed and truncated blobs; the
  encode-side capability and error-code invariants. The BBQR header (`B$`, generic
  binary file type `B`) is asserted by the unit tests in `src/bbqr.rs`, the only
  place the raw part string is reachable.

### Acceptance (Phase 2)
- [x] `cargo test -p bwk-qr-protocol --all-features` and
  `cargo test -p bwk-qr --features protocol` green; clippy/fmt clean
- [x] `cargo build -p bwk-qr-protocol --target riscv32imac-unknown-none-elf` green,
  with and without `ffi`; `cargo tree` shows no dependency
- [x] All four message pairs round-trip through the codec + BBQR (encode ->
  reassemble)
- [x] Version-compat tests pass both directions (older<->newer)

---

## Phase 3 - Scanning (feature `scan`) + Silent camera

### bwk-qr
- [x] Add `quircs` (feature `scan`) to `qr/Cargo.toml`
- [x] `scan.rs` (internal): `Scanned { text, bytes }`,
  `scan(&Image, bool, usize)`; assert `data.len() == width*height` ->
  `BadFrame`; run `Quirc::identify` + `Code::decode`, skipping a candidate that
  fails to decode; `find_inverted` inverts and rescans. Public plain-scan path is
  `Decoder::process()`
- [x] `tests/scan.rs`: render generated `Image` (already grayscale) -> decode ->
  equals original (needs `gen` + `scan`); inverted frames; a frame with no code
  decodes to nothing; the pixel bound is enforced

### Silent (camera; gated)
- [ ] Enable `scan`; bump rev + lock/flake hashes
- [ ] Bridge: `fn qr_decode_grayscale(gray: &[u8], width: u32, height: u32) ->
  Vec<String>` via a `Decoder` (`process` the frame)
- [ ] `CMakeLists.txt`: `option(SILENT_ENABLE_CAMERA OFF)`; when ON
  `find_package(Qt6 ... Multimedia MultimediaWidgets)`, link `Qt6::Multimedia`,
  define `SILENT_ENABLE_CAMERA=1`, update whole-archive lists
- [ ] **flake.nix / qt_static**: static `qtmultimedia` for Linux first, then
  Windows + macOS (dominant risk)
- [ ] `src/catalog/scan/CameraScanner.{h,cpp}` (`QCamera`+`QVideoSink`,
  `onVideoFrameChanged` -> grayscale -> `qr_decode_grayscale`)
- [ ] `src/views/modals/ScanQrModal.{h,cpp}` (`init`/`doConnect`/`view`,
  `onDecoded`); wire a scan button into Send `OutputW` (`onScanAddress`) behind
  `#ifdef SILENT_ENABLE_CAMERA`

### Acceptance (Phase 3)
- [x] `cargo test -p bwk-qr --features "gen scan"` green (render->decode)
- [ ] Linux `-DSILENT_ENABLE_CAMERA=ON`; scanning a printed address QR fills the
  recipient field
- [ ] Static `qtmultimedia` proven on Linux (Windows/macOS tracked separately)

---

## Phase 3.5 - C binding for a bare-metal signer (feature `ffi`)

- [x] `#[repr(C)]` mirrors of the message tree in `qr-protocol/src/ffi/types.rs`,
  tagged unions keyed by the existing wire codes; `NULL`/`-1` for absent
- [x] `bwk_qr_request_decode` + `bwk_qr_request_free` (Rust to C) and
  `bwk_qr_response_encode` + `bwk_qr_buf_free` (C to Rust), the signer direction
- [x] Flat `int32_t` codes, `100+` reader, `200+` decode, `300+` encode, `400+` the
  binding, each carrying the static message the Rust `Display` uses
- [x] Hand-written `include/bwk_qr_protocol.h`; no cbindgen, no build script
- [ ] `tests/ffi.rs` over the `#[repr(C)]` types, so CI needs no C toolchain;
  `tests/layout.rs` pins every size and alignment against the header

### Acceptance (Phase 3.5)
- [ ] `cargo test -p bwk-qr-protocol --features ffi` green
- [ ] `cargo +nightly miri test` green over the raw-pointer paths, no leaks
- [ ] Every mirror type matches what a C compiler computes from the header
- [x] `examples/signer.c` compiles with `-Wall -Wextra`, links against a staticlib
  shim and round-trips a real request

---

## Phase 4 - Silent signing-flow integration

Wires goal 2 into Silent. Needs Phase 2 (protocol) + Phase 3 (camera/scan).

### bwk-qr
- [x] Confirm `Decoder` covers frame-loss / out-of-order / duplicate parts;
  add tests

### Silent
- [ ] Bridge: build a `Sign` request from the prepared PSBT + `Descriptor` entries ->
  `Encoder::encode_request` -> animated frames
- [ ] Bridge: opaque `Box<Decoder>` (`process`, `progress`, `error`; extract the
  signed PSBT from `response()`)
- [ ] Spike: can `bitcoin`/miniscript `finalize_mut()` finalize an
  externally-signed PSBT inside `account.rs`? If not, add a finalize-only helper to
  `bwk-sp` and **report before changing bwk-sp**
- [ ] Bridge: `Account::broadcast_signed_psbt(psbt_bytes: Vec<u8>) -> TxResult`
  (deserialize -> combine -> finalize -> extract_tx -> existing broadcast path)
- [ ] `src/catalog/scan/AnimatedQr.{h,cpp}` (reuses `QrCode`, `m_frame_timer`,
  ~250ms cycle) to display the request parts
- [ ] `src/views/modals/ExportSignRequestModal.{h,cpp}` (animate the `Sign`
  request) + `src/views/modals/ScanResponseModal.{h,cpp}` (N-of-M progress,
  reassemble the response)
- [ ] Wire into `Send.cpp` signer/broadcast steps (`onExportSignRequest`,
  `onScanSignedResponse` on a background `QThread`; reuse `onBroadcastResult`);
  keep the internal-sign path intact
- [ ] (optional, later) Address-verification, register-descriptor, get-xpubs flows
  reusing the same encode/scan plumbing

### Acceptance (Phase 4)
- [ ] Rust unit tests for `broadcast_signed_psbt` finalize/extract on Regtest
- [ ] End-to-end on Regtest: animate a `Sign` request, sign externally, scan the
  multi-part response, tx broadcasts with the expected txid
- [ ] Frame-loss handled gracefully (progress shown, re-scan possible)

---

## Cross-cutting / done-when-shipping
- [x] `cargo clippy --workspace --all-targets --all-features` + workspace-wide
  `cargo fmt --check` clean each phase
- [x] `gen` pulls a zero-dependency crate; `scan` adds only `quircs`' small tree;
  no `cc`, no build script
- [x] `protocol` wire format is versioned and append-only; the `since` convention is
  stated at the top of `qr-protocol/src/lib.rs` and matches `ENCODING.md`
- [x] `bwk-qr-protocol` stays `no_std` and dependency-free by default, and the C
  header stays in step with `ffi::types`
- [ ] Each bwk change: bump `silent/Cargo.toml` rev + refresh `Cargo.lock` +
  `flake.lock` + `flake.nix` `outputHashes`
- [x] PLAN.md and ROADMAP.md kept in sync with reality
