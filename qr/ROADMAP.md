# bwk-qr: roadmap & progress checklist

Single source of truth for progress. Check items off (`[x]`) as they land. Each
phase has explicit acceptance criteria; do not mark a phase done until every
criterion passes. See `PLAN.md` for the design these items implement.

The crate has two goals: (1) provide QR generation + scanning via pure-Rust
crates; (2) implement the miniscript QR signing-flow protocol with our own
hand-rolled, versioned binary encoding, framed over the `bbqr` crate. Phases below
build both, interleaved with Silent integration.

Legend: `[ ]` todo, `[~]` in progress, `[x]` done. Update this file in the same
commit as the work it describes.

---

## Phase 0 - Docs + empty crate skeleton (compiles, no logic)

### Docs
- [x] Write `qr/PLAN.md` (design)
- [x] Write `qr/ROADMAP.md` (this checklist)
- [x] Write `qr-protocol/ENCODING.md` (authoritative, transport-agnostic encoding)
- [ ] Have the docs reviewed by pythcoiner before any code

### Crate skeleton (only after docs are approved)
- [ ] Add `"qr"` to `members` in `bwk/Cargo.toml`
- [ ] `qr/Cargo.toml`: package `bwk-qr`; features `gen` (default), `scan`,
  `protocol` (implies `gen`+`scan`); optional deps `qrcodegen` (behind `gen`),
  `quircs` (behind `scan`),
  `bbqr` (`default-features = false`) + `bitcoin` (behind `protocol`); no serde/CBOR,
  no build script
- [ ] `qr/src/lib.rs`: module decls (`error`, `gen`, `scan`, `protocol`)
  feature-gated; crate docs stating the two goals
- [ ] `qr/src/error.rs`: `Error` enum (with `None` variant) + `Display`/`std::error::Error`
- [ ] `qr/src/{gen,scan}.rs` (internal helpers) and
  `qr/src/protocol/{mod,message,codec,frame}.rs` (public surface): types +
  signatures with `todo!()` bodies so the API compiles

### Acceptance (Phase 0)
- [ ] `cargo check -p bwk-qr` clean across: default, `--features scan`,
  `--features protocol`, `--all-features`
- [ ] `cargo clippy -p bwk-qr --all-features` clean; `cargo fmt --check` clean
- [ ] No Silent repo changes

---

## Phase 1 - Generation (feature `gen`) + Silent Receive QR

### bwk-qr
- [ ] Add `qrcodegen` (feature `gen`) to `qr/Cargo.toml`
- [ ] `gen.rs` (internal): `CorrectionLevel`, `Image`, `encode -> Option<Image>`;
  rasterize the qrcodegen modules (`get_module`) into a grayscale `Image`; `None` if
  too long
- [ ] Public plain-QR path `Encoder::from_str(&str) -> Vec<Image>` (in
  `frame.rs`/protocol): a single plain frame for anything that fits one QR
  (addresses always do), BBQR-animated only when too large for one code
- [ ] `tests/gen.rs`: known-vector sizes; long SP-address payload within v40
- [ ] `cargo test -p bwk-qr --features gen` green; clippy/fmt clean

### Silent (separate repo; bumps bwk rev)
- [ ] Add `bwk-qr` (features `gen`) to `silent/Cargo.toml`; bump `bwk` git `rev`
- [ ] Bridge: `struct Image { data, width, height }` +
  `fn qr_encode(data: &str) -> Image` via `Encoder::from_str` (empty `data` =
  failure) in `lib.rs` + `account.rs`
- [ ] `./build.sh` regenerates `lib/include/silent.h`
- [ ] Rewrite `src/catalog/panels/receive/QrCode.cpp::paintEvent` to blit the raster
  into a cached `QImage` (integer scale, quiet zone, blank fallback) instead of
  drawing modules
- [ ] Refresh `Cargo.lock`, `flake.lock`, `flake.nix` `outputHashes`

### Acceptance (Phase 1)
- [ ] `cargo test -p bwk-qr` green; clippy/fmt clean
- [ ] `just br`: Receive shows real QR for SP / segwit / taproot addresses
- [ ] Phone wallet scan decodes to exactly the displayed address
- [ ] Address-history modal QRs render; empty state still blank
- [ ] Crisp at 256px (modal) and `Size::M` (panel)

---

## Phase 2 - Signing-flow protocol core (feature `protocol`)

Pure bwk-qr; no camera, no Silent UI. The heart of goal 2.

- [ ] Pin `bbqr` version; confirm `Split` / `SplitOptions` / `ContinuousJoiner` /
  `FileType::Binary` API against the pinned release
- [ ] `protocol/message.rs`: `Request`/`Response` enums + payload structs for Get
  Xpubs, Register Descriptor, Address Verification, Signing (per PLAN 4.1); plain
  types (no serde); reuse `bitcoin` types (`Psbt`, `Xpub`, `Fingerprint`,
  `DerivationPath`); descriptors as `DescriptorBody` (BIP-380 string or BIP-388
  keys+policy); silent-payment `SpShare`/`SpOutput` in the Signing response
- [ ] `protocol/codec.rs`: internal binary encode/decode implementing `ENCODING.md`
  exactly; varint (CompactSize) + primitive helpers; envelope magic + `version` +
  `msg_type`; append-only field ordering with each field's `since` documented inline
- [ ] Validation: at most one `bip380`/`bip388` per Register request; signing
  response is exactly one of Psbt / Signatures; `SigKind` mapping (incl. silent
  payment); truncated/garbage -> `Error::Decode`
- [ ] `protocol/frame.rs`: `Encoder` (`config`; `from_str` for plain QRs;
  `from_request`/`from_response` -> `Vec<Image>` via
  `Split::try_from_data(.., FileType::Binary, ..)`); `Config` with sane QR-density
  defaults
- [ ] `protocol/frame.rs`: `Decoder` wrapping `ContinuousJoiner` + `quircs` scan
  (`process(&Image)`; `progress`/`error`/`request`/`response`/`string`/`reset`,
  running the codec on `Joined.data`; `string()` returns decoded plain-QR text)
- [ ] Map `bbqr` errors into `Error::Bbqr`
- [ ] `tests/protocol.rs`: round-trip per message (`Encoder::from_* -> render ->
  Decoder`); version-compat (internal codec: v1 ignores a synthetic newer trailing
  field; newer reads a v1 blob); ordered + shuffled frames; malformed frame ->
  `Decoder::error()`; assert BBQR header (`B$`, `FileType::Binary`)

### Acceptance (Phase 2)
- [ ] `cargo test -p bwk-qr --features protocol` green; clippy/fmt clean
- [ ] All four message pairs round-trip through the codec + BBQR (encode ->
  reassemble)
- [ ] Version-compat tests pass both directions (older<->newer)

---

## Phase 3 - Scanning (feature `scan`) + Silent camera

### bwk-qr
- [ ] Add `quircs` (feature `scan`) to `qr/Cargo.toml`
- [ ] `scan.rs` (internal): `Decoded { text, bytes }`,
  `decode(&Image, bool)`; assert `data.len() == width*height` ->
  `BadFrame`; run `Quirc::identify` + `Code::decode`; `find_inverted` inverts and
  rescans. Public plain-scan path is `Decoder::string()`
- [ ] `tests/scan.rs`: render generated `Image` (already grayscale) -> decode ->
  equals original (needs `gen` + `scan`)

### Silent (camera; gated)
- [ ] Enable `scan`; bump rev + lock/flake hashes
- [ ] Bridge: `fn qr_decode_grayscale(gray: &[u8], width: u32, height: u32) ->
  Vec<String>` via a `Decoder` (`process` the frame, read `string()`)
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
- [ ] `cargo test -p bwk-qr --features "gen scan"` green (render->decode)
- [ ] Linux `-DSILENT_ENABLE_CAMERA=ON`; scanning a printed address QR fills the
  recipient field
- [ ] Static `qtmultimedia` proven on Linux (Windows/macOS tracked separately)

---

## Phase 4 - Silent signing-flow integration

Wires goal 2 into Silent. Needs Phase 2 (protocol) + Phase 3 (camera/scan).

### bwk-qr
- [ ] Confirm `Decoder` covers frame-loss / out-of-order / duplicate parts;
  add tests

### Silent
- [ ] Bridge: build a `Sign` request from the prepared PSBT + wallet refs ->
  `Encoder::from_request` -> animated frames
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
- [ ] `cargo clippy -p bwk-qr --all-features` + `cargo fmt --check` clean each phase
- [ ] `gen` pulls a zero-dependency crate; `scan` adds only `quircs`' small tree;
  no `cc`, no build script
- [ ] `protocol` wire format is versioned and append-only; each field's
  introduction version documented in `codec.rs`, matching `ENCODING.md`
- [ ] Each bwk change: bump `silent/Cargo.toml` rev + refresh `Cargo.lock` +
  `flake.lock` + `flake.nix` `outputHashes`
- [ ] PLAN.md and ROADMAP.md kept in sync with reality
