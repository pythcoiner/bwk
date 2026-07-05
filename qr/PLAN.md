# bwk-qr: design plan

`bwk-qr` is a Rust crate in the `bwk` workspace (`bwk/qr/`, package `bwk-qr`) with
**two goals**:

1. **QR generation and scanning** as internal, pure-Rust building blocks:
   *generation* via `qrcodegen` (Project Nayuki's own Rust port) and
   *scanning/decoding* from a grayscale frame via `quircs` (a `quirc` port). These
   primitives are crate-internal; the only public surface is the protocol
   `Encoder`/`Decoder` (plus `Image`, the message enums, and `Error`).
2. **Implement the QR-based signing-flow protocol** for the miniscript context
   specified in [`ENCODING.md`](../qr-protocol/ENCODING.md): the request/response payloads (Get
   Xpubs, Register Descriptor, Address Verification, Signing), serialized with our
   own hand-rolled, versioned binary encoding and chunked across animated QR frames
   using the `bbqr` crate.

The two goals are layered and independently usable: a consumer enables the `gen`
feature and calls `Encoder::from_str` for a plain QR (zero extra Rust deps), or the
full `protocol` (which adds only `bbqr` for framing and `bitcoin` for wallet types)
for the signing flow. The first consumer is the
Silent wallet, which reaches the crate through Silent's existing CXX (C++ <->
Rust) bridge.

> Status: this document and `ROADMAP.md` are the deliverable for now. No crate
> skeleton or code exists yet. `ROADMAP.md` is the authoritative, living
> checklist; this file is the authoritative design. Keep both in sync.

---

## 1. Scope

### In scope
- Goal 1: safe `encode` (bytes -> `Image`) and `decode` (frame ->
  payloads) over pure-Rust crates (`qrcodegen`, `quircs`).
- Goal 2: typed protocol messages for the four payload pairs; our own versioned
  binary wire format; BBQR framing/reassembly via the `bbqr` crate; a streaming
  decoder for animated multi-part scans.

### Out of scope
- No camera capture, windowing, or UI. The consumer owns capture and rendering.
- No signing, PSBT finalization, or descriptor evaluation. The protocol layer
  *carries* PSBTs/descriptors; their semantics live in the consumer (Silent's
  `account.rs` / `bwk-sp`, or a hardware device).
- No UR (`ur:...`) framing in v1. BBQR is the chosen multi-part format.
- No async, no threads, no global state. The only stateful type is the protocol
  stream decoder (which wraps `bbqr`'s `ContinuousJoiner`).

### A note on the protocol's maturity
The protocol is an explicit **draft**. `bwk-qr` defines its own wire format (section
4.2) and framing (BBQR); the authoritative byte-level contract is `ENCODING.md`. The
format carries a version and is append-only, so it can evolve without an external
schema and stay independent of any third-party serialization. New fields are added
by appending (see the candidates listed in `ENCODING.md`).

---

## 2. Design principles

**Layer 1 (QR primitives) is dependency-minimal and firm:**
1. Pure Rust: generation via `qrcodegen` (Nayuki's own port, **zero dependencies**),
   scanning via `quircs` (a faithful `quirc` port). No vendored C, no FFI, no `cc`,
   no system libs, no submodules.
2. No `unsafe` anywhere in the crate; gen/scan are internal helpers, safe like the
   rest of the surface.
3. The `gen` feature adds **no runtime dependencies**. The `scan` feature adds only
   `quircs` and its small tree (`num-derive`, `num-traits`, `thiserror`); no `image`
   at runtime.
4. Both crates are 1:1 ports of the C libraries first considered, so behaviour is on
   par: multi-code-per-frame is preserved; inverted (white-on-black) QRs are not
   auto-detected (the internal scan helper inverts and rescans).

**Layer 2 (protocol) stays independent too:**
5. The wire encoding is **hand-rolled binary**, defined here. No `serde`, no CBOR,
   no external serialization crate. We own the byte layout and the versioning.
6. The `protocol` feature pulls `bbqr` (framing, not reimplemented) and
   `bitcoin` (for `Psbt`/`Xpub`/`Fingerprint`/`DerivationPath` and their existing
   byte serializations), and implies `gen` + `scan` (the Encoder renders QR, the
   Decoder scans it). Descriptors are carried as validated strings (the device
   parses them), so no `miniscript` dependency.

**Both layers:** portable across the targets Silent ships (Linux, Windows/mingw,
macOS x86_64/arm64) under the existing Nix toolchain, with no per-target C tweaks.

---

## 3. Goal 1: QR primitives (internal, pure Rust)

### 3.1 Crates
- **Generation, `qrcodegen` 1.8.0** (MIT): Nayuki's own Rust port, **zero runtime
  dependencies**. `QrCode::encode_binary(&[u8], QrCodeEcc)` for byte mode;
  `get_module(x, y) -> bool` and `size() -> i32` to rasterize the modules. Behind
  the `gen` feature.
- **Scanning, `quircs` 0.10.3** (MIT): a faithful `quirc` port.
  `Quirc::identify(width, height, &[u8])` consumes a raw 8-bit grayscale frame (no
  `image` crate) and yields an iterator of codes; `Code::decode()` returns the
  payload as `Vec<u8>`, so binary content is preserved. Adds `num-derive`,
  `num-traits`, `thiserror`. Behind the `scan` feature.

Both are 1:1 ports of the C libraries first considered (Nayuki qrcodegen, quirc), so
behaviour parity is high. Neither pulls `image`, `nalgebra`, or a C toolchain. BBQR
is handled by the `bbqr` crate (section 4.2), not reimplemented.

### 3.2 Implementation notes
- **Generation:** `encode_binary` takes `&[u8]` and returns an owned `QrCode`; we
  rasterize its modules (`get_module`) into an `Image` (row-major grayscale, one
  pixel per module plus the standard quiet zone, `0` = dark, `255` = light).
  Byte-mode capacity is the v40 limit (2953 bytes); larger payloads are chunked at
  the BBQR layer, not here. `DataTooLong` -> `Error::TooLong`.
- **Scanning:** `frame.data` must be exactly `width*height` bytes, else
  `Error::BadFrame`. Feed it to `Quirc::identify`, then collect each decoded `Code`
  into a `Decoded { text, bytes }` (`bytes` is the raw payload; `text` is its
  lossy-UTF-8 view). quircs does not auto-detect inverted (white-on-black) frames,
  so `find_inverted` inverts the buffer and rescans.

### 3.3 Internal primitives

Only `Image` is public. Generation and scanning are crate-internal helpers used by
the `Encoder`/`Decoder`; they are not exposed.

```rust
// public: the single raw-data type crossing the API in both directions
pub struct Image { pub data: Vec<u8>, pub width: u32, pub height: u32 }

// internal (feature `gen`): bytes -> QR Image
enum CorrectionLevel { Low, Medium, Quartile, High }       // -> qrcodegen::QrCodeEcc
fn encode(data: &[u8], level: CorrectionLevel) -> Option<Image>;   // None if too long

// internal (feature `scan`): camera Image -> decoded payloads
struct Decoded { text: String, bytes: Vec<u8> }
fn decode(frame: &Image, find_inverted: bool) -> Vec<Decoded>;
```

`Image` is an 8-bit grayscale raster, row-major, `width*height` bytes (camera frame
in, QR frame out; generated QR `0` = dark, `255` = light, one pixel per module plus
the quiet zone; the consumer scales for display). `find_inverted` rescans an inverted
copy (quircs has no built-in inversion).

---

## 4. Goal 2: the signing-flow protocol

### 4.1 Payload model (from ENCODING.md)
Four request/response pairs. Software wallet -> device for all requests. Field
names mirror ENCODING.md; types use `bitcoin` where natural.

1. **Get Xpubs**
   - Request: `derivation_paths: Vec<DerivationPath>`
   - Response: `xpubs: Vec<Xpub>` (order matches request), `fingerprint:
     Fingerprint`, `model: String` (16-byte NUL-padded), `version: Version { major:
     u16, minor: u16, patch: u32, flag: ReleaseFlag }`, `capabilities: Capabilities`
     (32-bit flags). All mandatory.
2. **Register Descriptor**
   - Request: `wallet: String` (alias), `descriptor: Option<DescriptorBody>` (absent
     = status query); `DescriptorBody` is a `FORM` (BIP-380 descriptor, or BIP-388
     wallet policy = keys vector + policy template) plus its body
   - Response: `wallet: String`, `registered: Option<bool>`, `por: Option<Vec<u8>>`
     (proof of registration). A failed registration is an error response, not a body
     field.
3. **Address Verification**
   - Request: `wallet: String`, `deriv: DerivationPath`, `address: Option<String>`,
     `descriptor: Option<DescriptorBody>`, `por: Option<Vec<u8>>`
   - Response: `uri: Option<String>` (BIP-21)
4. **Signing**
   - Request: `wallets: Vec<WalletRef { alias: String, descr: DescriptorBody, por:
     Option<Vec<u8>> }>`, `psbt: Psbt`, `want_kind: Option<Kind>` (advisory)
   - Response (either `Kind`):
     - `Psbt(Psbt)`, full PSBT with partial sigs and, for silent-payment sends, the
       BIP-375 shares/proofs and derived output scripts, or
     - `Signatures { sigs: Vec<SigEntry>, sp_shares: Vec<SpShare>, sp_outputs:
       Vec<SpOutput> }`. A `SigEntry` carries the key material the coordinator needs
       to place the signature in its PSBT, so its fields depend on the kind: segwit
       has `{ input, public_key, signature }`, tapkey `{ input, signature }`, taptree
       `{ input, xonly_public_key, tap_leaf_hash, signature }`. No control block is
       sent, the coordinator rebuilds it from the descriptor at finalize time.
       `SpShare { input: u32 (0xFFFFFFFF = aggregate), scan_key: [u8; 33],
       ecdh_share: [u8; 33], dleq_proof: [u8; 64] }`;
       `SpOutput { index: u32, script_pubkey: Vec<u8> }`

Modeled in Rust as two enums plus the payload structs:
```rust
pub enum Request  { None, GetXpubs(..), RegisterDescriptor(..), VerifyAddress(..), Sign(..) }
pub enum Response { None, Xpubs(..), Registration(..), AddressUri(..), Signed(..),
                    Error { code: u8, message: String } }  // 1-byte code, 32-byte msg
```
A `Response::Error` encodes with the `STATUS` bit set and the standard error body
(`ENCODING.md`); the ok variants encode with `STATUS` = 0.

### 4.2 Wire format (hand-rolled, versioned, append-only)

We define our own binary encoding. It is independent (no serialization crate),
compact, and forward/backward compatible by construction. **`ENCODING.md` is the
authoritative, transport-agnostic byte-level contract**; this section is only a
design summary, so any discrepancy resolves in favor of `ENCODING.md`.

In brief (full detail in `ENCODING.md`):
- A message is `MAGIC` (ASCII "BIPXXX") + `VERSION` (1 byte) + `MSG_TYPE` (1 byte)
  + `REQUEST_ID` (16 opaque bytes, echoed by the responder) + a per-type `BODY`.
- `MSG_TYPE` packs `DIRECTION` (bit 7) | `STATUS` (bit 6, ok/error) | `TYPE` (bits
  5-0). An error response (`STATUS` = 1) carries the standard error body: a 1-byte
  `ERROR_CODE` (global table, `0xFF` vendor-specific) + a 32-byte NUL-padded
  `ERROR_MESSAGE`.
- Counts and lengths use compact size; fixed multi-byte integers are big-endian;
  optional fields use a 1-byte presence prefix; vectors use a compact-size count.
- Domain fields: `STRING`/`BYTES` (compact-size length + bytes), `FINGERPRINT` (4),
  `XPUB` (78, BIP-32), `PSBT` (BIP-174 bytes), `DERIVATION_PATH` (1-byte child count
  + 4-byte big-endian children), `MODEL` (16-byte NUL-padded), `VERSION` (8-byte
  semver), `CAPABILITIES` (4-byte bitfield), `DESCRIPTOR_BODY` (`FORM` + body).
- `BODY` is append-only: fields ordered by their introduction version; a decoder
  reads the fields it knows and ignores trailing bytes from newer versions. New
  versions may only append fields.

The message bodies, value tables (`TYPE`, `FORM`, `KIND`, `SIG_KIND`, error codes,
capability bits), the versioning and error rules, and worked byte examples are all in
`ENCODING.md`.

### 4.3 Framing: BBQR via the `bbqr` crate
The encoded message bytes are split into animated parts, tagged
`FileType::Binary` (our own format, not PSBT/CBOR):
```rust
use bbqr::{split::{Split, SplitOptions}, file_type::FileType,
           continuous::{ContinuousJoiner, ContinuousJoinResult},
           header::{Encoding, Version}};
let split = Split::try_from_data(&wire, FileType::Binary, SplitOptions {
    encoding: Encoding::Zlib,            // BBQR-level deflate; falls back if larger
    min_split_number: 1, max_split_number: 1295,
    min_version: Version::V03, max_version: Version::V20, // bound QR density
})?;
let parts: Vec<String> = split.parts;    // Encoder rasterizes these into Vec<Image>
```
BBQR owns base32/zlib/chunking/ordering and the 8-char `B$..` header; we do not
reimplement any of it. Raw PSBTs (for signers that speak plain BBQR rather than
this protocol) can also be framed directly as `FileType::Psbt`.

Reassembly wraps `bbqr::ContinuousJoiner`: feed scanned strings until
`ContinuousJoinResult::Complete(Joined)`, then decode `Joined.data` with our codec
back into a `Request`/`Response`.

### 4.4 External API (`src/protocol/`)

Two structs plus `Config`. State is read through accessors that return the
`None`-variant enums, so no `Result` appears in the surface.

```rust
#[derive(Debug, Clone)]
pub struct Config { pub max_qr_version: u8, /* BBQR encoding, split bounds, ... */ }
impl Default for Config { /* sane density defaults */ }

// Build the animated frames for a message.
pub struct Encoder { /* ... */ }
impl Encoder {
    pub fn config(cfg: Config) -> Self;
    pub fn from_str(&self, s: &str)            -> Vec<Image>; // plain string, e.g. an address
    pub fn from_request(&self, req: Request)   -> Vec<Image>; // one Image per frame
    pub fn from_response(&self, res: Response) -> Vec<Image>;
}

// Feed grayscale camera frames; scans QR -> reassembles BBQR -> decodes the message.
pub struct Decoder { /* ... */ }
impl Decoder {
    pub fn new() -> Self;
    pub fn process(&mut self, frame: &Image);    // one grayscale camera frame
    pub fn progress(&self) -> (u8, u8);          // (parts_seen, parts_total)
    pub fn error(&self) -> Error;                // Error::None while healthy
    pub fn request(&self) -> Request;            // Request::None until complete
    pub fn response(&self) -> Response;          // Response::None until complete
    pub fn string(&self) -> String;              // decoded plain (non-protocol) QR text; empty until one arrives
    pub fn reset(&mut self);
}
```

`Encoder::from_*` returns one `Image` per animated frame, ready to blit. Dims travel
with each `Image`, so frame size may vary and `Decoder::new()` needs no dimensions.
`Decoder::process` takes one grayscale camera frame; once enough parts arrive,
`request()` or `response()` yields the decoded message (whichever the completed
message was) and `error()` reports any failure. Use `from_str`/`string()` for plain
(non-protocol) address QRs; `from_request`/`from_response` with
`request()`/`response()` for the signing-flow messages.

---

## 5. Crate layout

```
bwk/qr/
  Cargo.toml
  PLAN.md            <- this file
  ROADMAP.md         <- the living checklist
  src/
    lib.rs           <- public API, re-exports, crate docs
    error.rs         <- Error enum
    gen.rs           <- safe QR generation API           (feature: gen)
    scan.rs          <- safe grayscale decode API         (feature: scan)
    protocol/
      mod.rs         <- Request/Response enums, re-exports (feature: protocol)
      message.rs     <- payload structs (plain types, no serde)
      codec.rs       <- hand-rolled binary encode/decode + version rule + compactsize
      frame.rs       <- Encoder + Decoder (BBQR split/join)
  tests/
    gen.rs           <- encode (+ self-decode where scan on), known vectors
    scan.rs          <- render -> grayscale -> decode round-trip (feature: scan)
    protocol.rs      <- codec round-trip; version compat; encode -> scan reassembly
```

---

## 6. Cargo manifest

```toml
[package]
name = "bwk-qr"
edition = "2021"

[features]
default  = ["gen"]
gen      = ["dep:qrcodegen"]  # zero-dependency generation
scan     = ["dep:quircs"]     # quirc port; pulls num-derive/num-traits/thiserror
protocol = ["gen", "scan", "dep:bbqr", "dep:bitcoin"]

[dependencies]
qrcodegen = { version = "1.8", optional = true }             # zero deps
quircs    = { version = "0.10", optional = true }            # grayscale decode
bbqr      = { version = "*", optional = true, default-features = false } # pin a version
bitcoin   = { workspace = true, optional = true }            # Psbt/Xpub/etc.
```
- `gen` adds a zero-dependency crate; `scan` adds only `quircs`' small tree;
  `protocol` adds `bbqr` (framing) and `bitcoin` (wallet types) and implies
  `gen` + `scan` (the Encoder/Decoder do full QR I/O). No `serde`, no CBOR,
  no C toolchain.
- Confirm and pin the exact `bbqr` version at implementation time.

---

## 7. Build

Both `qrcodegen` and `quircs` are pure Rust: no `build.rs`, no `cc`, no C toolchain.
The feature-gated optional deps keep unused code out (`gen` -> `qrcodegen`, `scan` ->
`quircs`, `protocol` -> `bbqr`/`bitcoin`). Cross-compilation to the Silent targets is
whatever `cargo` does for a pure-Rust crate, nothing target-specific.

---

## 8. Errors (`src/error.rs`)

```rust
#[derive(Debug, Clone)]
pub enum Error {
    None,            // no error
    TooLong,         // payload exceeds QR capacity
    Encode,          // qrcodegen failed
    BadFrame,        // grayscale buffer size != width*height
    Bbqr(String),    // bbqr crate error (split/join)
    Decode(String),  // truncated/invalid wire bytes
    Protocol(String) // payload validation (e.g. >1 BIP form, bad sig kind)
}
// impl Display + std::error::Error (Error::None renders as "no error").
// No Result alias: fallible entry points expose state via accessors / Option.
```

---

## 9. Consumer integration

### 9.1 Silent (existing CXX bridge)
C++ never links `bwk-qr` directly. Silent's `silent` crate depends on `bwk-qr`
and re-exposes a thin surface in its `#[cxx::bridge]` (`silent/src/lib.rs`, impl in
`account.rs`), per Silent's FFI rules (no `Result` across the bridge;
sentinel/empty returns). Planned surface (added per phase):
- `struct Image { data: Vec<u8>, width: u32, height: u32 }` + `fn qr_encode(data:
  &str) -> Image`, implemented via `Encoder::from_str` (take the single plain frame;
  empty `data` = failure); the Receive screen blits the raster.
- `fn qr_decode_grayscale(gray: &[u8], width: u32, height: u32) -> Vec<String>` for
  the camera scanner, implemented via a `Decoder` (`process` the frame, read
  `string()`).
- Signing-flow surface: build a `Sign` request -> `Encoder::from_request` -> frames
  to animate; an opaque `Box<Decoder>` (`process`/`progress`/`error`/`response`) to
  reassemble the response; then `Account::broadcast_signed_psbt(psbt: Vec<u8>)`.

Each bwk change bumps the pinned `bwk` git `rev` in `silent/Cargo.toml`, refreshes
`Cargo.lock`, and updates `flake.lock` + the `importCargoLock` `outputHashes`.

### 9.2 Other Rust callers
```rust
// plain QR (generation only, zero transitive deps):
use bwk_qr::protocol::{Encoder, Config};
let frames = Encoder::config(Config::default()).from_str("bc1q..."); // Vec<Image>

// protocol:
use bwk_qr::protocol::{Encoder, Config};
let frames = Encoder::config(Config::default()).from_request(req); // animate frames
```

---

## 10. Testing

- `gen`/`scan` correctness is exercised via internal (crate-private) tests and
  end-to-end through `Encoder`/`Decoder`; the primitives are not called publicly.
  The plain-string round-trip is `Encoder::from_str -> Decoder::process ->
  string()`. Internally: known-vector sizes; long (~100+ char) address encodes
  within v40; the generated grayscale `Image` feeds the scan helper directly and
  equals input; synthetic grayscale frames (quiet zone + scale).
- `protocol`:
  - round-trip per message (`Encoder::from_* -> render -> Decoder` yields the
    original);
  - **version compatibility** (internal codec tests): a v1 decoder reads a synthetic
    v(next) blob's known fields and ignores the tail; a v(next) decoder reads a v1
    blob (missing trailing field) correctly;
  - truncated/garbage frame -> `Decoder::error()` is `Error::Decode`; `>1` BIP form
    -> `Error::Protocol`;
  - `Encoder::from_request` yields grayscale frames that feed (ordered + shuffled)
    into a `Decoder` directly; `request()` equals the original;
  - assert the BBQR header (`B$`, `FileType::Binary`).
- Offline, deterministic, plain `cargo test -p bwk-qr`. Build/test matrix:
  `--no-default-features --features gen`, `--features scan`, `--features protocol`,
  `--all-features`. `cargo clippy --all-features` clean.

---

## 11. Risks and open questions

- **Protocol is a draft.** Several fields are our choice; bump the wire `version`
  and only append fields. Add the candidates listed in `ENCODING.md` (spend-path
  selection, multi-descriptor registration) as new versions.
- **Wire-format spec discipline.** The append-only rule only holds if existing
  fields are never reordered or resized; enforce this in review and document each
  field's introduction version in `codec.rs`.
- **`bbqr` crate version/API.** Confirm `Split`/`ContinuousJoiner`/`FileType` and
  pin a version; map its errors into `Error::Bbqr`.
- **`bitcoin` byte serializations.** Use stable forms (`Psbt::serialize`, 78-byte
  `Xpub`, 4-byte `Fingerprint`); keep these out of the version-evolving region by
  treating them as opaque `bytes`/fixed fields.
- **UR support** deferred; revisit if a target signer needs `ur:` instead of BBQR.
