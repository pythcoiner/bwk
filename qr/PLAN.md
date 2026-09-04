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
   using BBQR generic-binary framing.

The two goals are layered and independently usable: a consumer enables the `gen`
feature and calls `Encoder::encode_text` for a plain QR, or the full `protocol`
(which adds `bitcoin` for wallet types)
for the signing flow. The first consumer is the
Silent wallet, which reaches the crate through Silent's existing CXX (C++ <->
Rust) bridge.

> Status: the crate skeleton, QR generation/scanning, protocol message model,
> hand-written codec, and generic-binary BBQR framing are implemented. The wire
> magic still uses `BIPXXX` until a BIP number is assigned.

---

## 1. Scope

### In scope
- Goal 1: safe `encode` (bytes -> `Image`) and `decode` (frame ->
  payloads) over pure-Rust crates (`qrcodegen`, `quircs`).
- Goal 2: typed protocol messages for the four payload pairs; our own versioned
  binary wire format; BBQR framing/reassembly; a streaming
  decoder for animated multi-part scans.

### Out of scope
- No camera capture, windowing, or UI. The consumer owns capture and rendering.
- No signing, PSBT finalization, or descriptor evaluation. The protocol layer
  *carries* PSBTs/descriptors; their semantics live in the consumer (Silent's
  `account.rs` / `bwk-sp`, or a hardware device).
- No UR (`ur:...`) framing in v1. BBQR is the chosen multi-part format.
- No async, no threads, no global state. The only stateful type is the protocol
  stream decoder.

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
   at runtime. Every feature is on by default, so a consumer that wants generation
   alone builds with `--no-default-features --features gen`.
4. Both crates are 1:1 ports of the C libraries first considered, so behaviour is on
   par: multi-code-per-frame is preserved; inverted (white-on-black) QRs are not
   auto-detected (the internal scan helper inverts and rescans).

**Layer 2 (protocol) stays independent too:**
5. The wire encoding is **hand-rolled binary**, defined here. No `serde`, no CBOR,
   no external serialization crate. We own the byte layout and the versioning.
6. The `protocol` feature pulls `bwk-qr-protocol` and implies `gen` + `scan` (the
   Encoder renders QR, the Decoder scans it). The codec itself carries `Psbt`,
   `Xpub`, `Fingerprint` and `DerivationPath` as their byte serializations rather
   than as `bitcoin` types, so it needs no dependency at all; `bwk-qr` turns on its
   `bitcoin` feature for the adapters. Descriptors are carried as validated strings
   (the device parses them), so no `miniscript` dependency either.

**Both layers:** portable across the targets Silent ships (Linux, Windows/mingw,
macOS x86_64/arm64) under the existing Nix toolchain, with no per-target C tweaks.

---

## 3. Goal 1: QR primitives (internal, pure Rust)

### 3.1 Crates
- **Generation, `qrcodegen` 1.8.0** (MIT): Nayuki's own Rust port, **zero runtime
  dependencies**. `QrCode::encode_text` for plain payloads and
  `encode_segments_advanced` with an alphanumeric segment for BBQR parts;
  `get_module(x, y) -> bool` and `size() -> i32` to rasterize the modules. Behind
  the `gen` feature.
- **Scanning, `quircs` 0.10.3** (MIT): a faithful `quirc` port.
  `Quirc::identify(width, height, &[u8])` consumes a raw 8-bit grayscale frame (no
  `image` crate) and yields an iterator of codes; `Code::decode()` returns the
  payload as `Vec<u8>`, so binary content is preserved. Adds `num-derive`,
  `num-traits`, `thiserror`. Behind the `scan` feature.

Both are 1:1 ports of the C libraries first considered (Nayuki qrcodegen, quirc), so
behaviour parity is high. Neither pulls `image`, `nalgebra`, or a C toolchain. BBQR
generic-binary framing is implemented in the crate because the published `bbqr`
crate does not expose the generic binary file type.

### 3.2 Implementation notes
- **Generation:** qrcodegen returns an owned `QrCode`; we rasterize its modules
  (`get_module`) into an `Image` (row-major grayscale with a quiet zone, `0` =
  dark, `255` = light). Generated images use a small module scale so the scanner
  can consume them directly.
  A plain payload takes the smallest version that fits within
  `Config::max_qr_version`; a BBQR part is pinned at `max_qr_version` so every
  animated frame renders at the same size. Payloads too large for one code are
  chunked at the BBQR layer, not here. `DataTooLong` -> `Error::TooLong`.
- **Scanning:** `frame.data` must be exactly `width*height` bytes, else
  `Error::BadFrame`. Feed it to `Quirc::identify`, then collect each decoded `Code`
  into a `Scanned { text, bytes }` (`bytes` is the raw payload; `text` is its
  lossy-UTF-8 view); a candidate code that fails to decode is skipped, since a
  camera frame routinely holds partial or blurred codes. quircs does not
  auto-detect inverted (white-on-black) frames, so `find_inverted` inverts the
  buffer and rescans.

### 3.3 Internal primitives

Only `Image` is public. Generation and scanning are crate-internal helpers used by
the `Encoder`/`Decoder`; they are not exposed.

```rust
// public: the single raw-data type crossing the API in both directions
pub struct Image { pub data: Vec<u8>, pub width: u32, pub height: u32 }

// internal (feature `gen`): bytes -> QR Image
enum CorrectionLevel { Low, Medium, Quartile, High }       // -> qrcodegen::QrCodeEcc
fn encode_text(data: &str, level: CorrectionLevel, max_version: u8) -> Result<Image, Error>;

// internal (feature `scan`): camera Image -> decoded payloads
struct Scanned { text: String, bytes: Vec<u8> }
fn scan(frame: &Image, find_inverted: bool, max_pixels: usize) -> Result<Vec<Scanned>, Error>;
```

`Image` is an 8-bit grayscale raster, row-major, `width*height` bytes (camera frame
in, QR frame out; generated QR `0` = dark, `255` = light, scaled modules plus the
quiet zone). `find_inverted` rescans an inverted copy (quircs has no built-in
inversion).

---

## 4. Goal 2: the signing-flow protocol

### 4.1 Payload model (from ENCODING.md)
Four request/response pairs. Software wallet -> device for all requests. Field
names mirror ENCODING.md; the leaf types are the byte-level ones from
`qr-protocol/src/types.rs`, with `bitcoin` adapters behind a feature.

1. **Get Xpubs**
   - Request: `derivation_paths: Vec<DerivationPath>`
   - Response: `xpubs: Vec<Xpub>` (78 bytes each, order matches request), `fingerprint:
     Fingerprint`, `model: String` (16-byte NUL-padded), `version: Version { major:
     u16, minor: u16, patch: u32, flag: ReleaseFlag }`, `capabilities: Capabilities`
     (32-bit flags). All mandatory.
2. **Register Descriptor**
   - Request: `descriptor_alias: String`, `descriptor: Option<DescriptorBody>`
     (absent = status query); `DescriptorBody` is a `FORM` (BIP-380 descriptor, or
     BIP-388 wallet policy = keys vector + policy template) plus its body
   - Response: `descriptor_alias: String`, `registered: Option<bool>`,
     `stored: Option<bool>` (device persisted the descriptor under the alias, so
     later requests may reference it by alias alone; absent means the device does
     not report it and the wallet must assume it did not), `por: Option<Vec<u8>>`
     (proof of registration). A failed registration is an error response, not a body field.
3. **Address Verification**
   - Request: `descriptor_alias: String`, `deriv: DerivationPath`,
     `address: Option<String>`, `descriptor: Option<DescriptorBody>`,
     `por: Option<Vec<u8>>`
   - Response: `uri: Option<String>` (BIP-21)
4. **Signing**
   - Request: `descriptors: Vec<Descriptor { alias: String, body: DescriptorBody,
     por: Option<Vec<u8>> }>`, `psbt: Vec<u8>` (BIP-174), `want_kind: Option<Kind>`
   - Response (either `Kind`):
     - `Psbt(Vec<u8>)`, full PSBT with partial sigs and, for silent-payment sends, the
       BIP-375 shares/proofs and derived output scripts, or
     - `Signatures(Vec<SignatureEntry>)`. `SignatureEntry` carries the key material
       the coordinator needs to place the signature in its PSBT, so its fields depend
       on the kind: `Ecdsa { input, public_key, signature }`, `TapKey { input,
       signature }`, `TapScript { input, xonly_public_key, tap_leaf_hash, signature
       }`. No control block is sent, the coordinator rebuilds it from the descriptor
       at finalize time. A silent-payment send has no signatures-only form, so the
       signer answers it with the `Psbt` variant whatever `want_kind` asked for

Modeled in Rust as a request id paired with a body enum, plus the payload structs:
```rust
// mod.rs
pub struct RequestId(pub [u8; 16]);
pub struct Request  { pub id: RequestId, pub body: request::Body }
pub struct Response { pub id: RequestId, pub body: response::Body }

// request.rs
pub enum Body { GetXpubs(..), RegisterDescriptor(..), VerifyAddress(..), Sign(..) }

// response.rs
pub enum Body { Xpubs(..), Registration(..), AddressUri(..), Signed(..), Error(ErrorBody) }
```
The responder echoes the request id, so a wallet can pair a response with the
request it sent. `response::Body::Error` encodes with the `STATUS` bit set and the
standard error body (`ENCODING.md`); the ok variants encode with `STATUS` = 0.
`ErrorCode` is typed: the eleven standard codes, `Vendor` (always `0xFF`, meaning
carried by the message), and `Unknown(u8)` for a code reserved for a future
version, which decodes and re-encodes unchanged so an older parser never rejects
a newer signer's code.

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
  `ERROR` (global table, `0xFF` vendor-specific) + a 32-byte NUL-padded
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

### 4.3 Framing: BBQR generic binary
The current implementation ships a small generic-binary BBQR subset using hex
encoding and the `B` file type because the published `bbqr` crate does not yet
expose `FileType::Binary`. Raw PSBT framing can be added once a fixed upstream
or pinned fork is available.

Reassembly accepts shuffled frames, rejects conflicting duplicates, and decodes
the joined bytes with the protocol codec. Completing a message clears the
reassembly state, so the same `Decoder` can take the next message straight away.

### 4.4 External API

Two structs plus `Config`, re-exported from the crate root. Rust callers get
`Result` and typed decoded values.

```rust
#[derive(Debug, Clone)]
pub struct Config { pub max_qr_version: u8, /* BBQR encoding, split bounds, ... */ }
impl Default for Config { /* sane density defaults */ }

// Build the animated frames for a message.
pub struct Encoder { /* ... */ }
impl Encoder {
    pub fn new(cfg: Config) -> Result<Self, Error>;
    pub fn encode_text(&self, s: &str) -> Result<Image, Error>;
    pub fn encode_request(&self, req: &Request) -> Result<Vec<Image>, Error>;
    pub fn encode_response(&self, res: &Response) -> Result<Vec<Image>, Error>;
}

// Feed grayscale camera frames; scans QR -> reassembles BBQR -> decodes the message.
pub struct Decoder { /* ... */ }
impl Decoder {
    pub fn new(cfg: Config) -> Result<Self, Error>;
    pub fn process(&mut self, frame: &Image) -> Result<Vec<Decoded>, Error>;
    pub fn progress(&self) -> Option<Progress>;
    pub fn reset(&mut self);
}
```

Protocol encoding returns one `Image` per animated frame, ready to blit. Dims
travel with each `Image`. `Decoder::process` takes one grayscale camera frame and
returns decoded plain text or protocol messages. Use `encode_text` for plain
address QRs and `encode_request`/`encode_response` for signing-flow messages.

---

## 5. Crate layout

```
The codec is its own crate so a bare-metal signer can take it without the QR layer.

```
bwk/qr/
  Cargo.toml
  PLAN.md            <- this file
  ROADMAP.md         <- the living checklist
  src/
    lib.rs           <- public API, re-exports, crate docs
    config.rs        <- Config (QR density and parser bounds)
    error.rs         <- Error enum
    image.rs         <- Image, the one raw-data type crossing the API
    gen.rs           <- QR generation                     (feature: gen)
    scan.rs          <- grayscale decode                  (feature: scan)
    encoder.rs       <- Encoder (plain text + protocol -> frames)  (feature: gen)
    decoder.rs       <- Decoder (frames -> Decoded, BBQR join)     (feature: scan)
    bbqr.rs          <- BBQR generic-binary split/join    (feature: protocol)
  tests/
    plain.rs         <- plain text render -> scan round-trip
    gen.rs           <- generation known vectors
    scan.rs          <- render -> grayscale -> decode round-trip (feature: scan)
    protocol.rs      <- encode -> render -> scan -> reassemble round-trip

bwk/qr-protocol/
  Cargo.toml
  ENCODING.md        <- the authoritative wire format
  include/
    bwk_qr_protocol.h  <- hand-written C header       (feature: ffi)
  examples/
    signer.c         <- hand-compiled C smoke test
  src/
    lib.rs           <- envelope types, Message, free encode/decode fns
    types.rs         <- the byte-level leaf types, plus bitcoin adapters
    request.rs       <- request payload structs (plain types, no serde)
    response.rs      <- response payload structs (plain types, no serde)
    reader.rs        <- byte cursor + compactsize
    encode.rs        <- hand-rolled binary encode
    decode.rs        <- hand-rolled binary decode + version rule
    ffi/             <- the C binding                 (feature: ffi)
      mod.rs         <- the four exported functions and their error codes
      types.rs       <- #[repr(C)] mirrors of the message tree
      owned.rs       <- the handle backing a decoded request
      read.rs        <- reads a C response back into the Rust types
  tests/
    codec.rs         <- codec round-trip, version compat, rejection cases
    vectors.rs       <- the ENCODING.md test vectors, checked both directions
    ffi.rs           <- the C binding, over the #[repr(C)] types
    layout.rs        <- pins the C layout so the header cannot drift
    *.json           <- the vectors themselves, one file per message group
```

---

## 6. Cargo manifest

```toml
[package]
name = "bwk-qr"
version = "0.0.1"
edition.workspace = true

[features]
default  = ["gen", "scan", "protocol"]
gen      = ["dep:qrcodegen"]  # zero-dependency generation
scan     = ["dep:quircs"]     # quirc port; pulls num-derive/num-traits/thiserror
protocol = ["gen", "scan", "dep:bwk-qr-protocol"]

[dependencies]
qrcodegen = { workspace = true, optional = true }             # zero deps
quircs    = { workspace = true, optional = true }             # grayscale decode
bwk-qr-protocol = { workspace = true, features = ["bitcoin"], optional = true }
```
- `gen` adds a zero-dependency crate; `scan` adds only `quircs`' small tree;
  `protocol` adds `bwk-qr-protocol` (itself dependency-free) and implies
  `gen` + `scan` (the Encoder/Decoder do full QR I/O). No `serde`, no CBOR,
  no C toolchain.
- A fixed `bbqr` release or pinned fork can replace the internal framing later.

---

## 7. Build

Both `qrcodegen` and `quircs` are pure Rust: no `build.rs`, no `cc`, no C toolchain.
The feature-gated optional deps keep unused code out (`gen` -> `qrcodegen`, `scan` ->
`quircs`, `protocol` -> `bwk-qr-protocol`). Cross-compilation to the Silent targets is
whatever `cargo` does for a pure-Rust crate, nothing target-specific.

`bwk-qr-protocol` goes further: it is `no_std` plus `alloc` with no dependency in its
default configuration, so it cross-compiles to a bare-metal signer target. That is why
it carries its own byte-level types instead of the `bitcoin` ones, which would drag in
`secp256k1-sys` and its vendored C.

---

## 8. Errors (`src/error.rs`)

```rust
#[derive(Debug, Clone)]
pub enum Error {
    TooLong,          // payload exceeds QR capacity
    BadFrame,         // grayscale buffer size != width*height
    Bbqr(bbqr::Error),          // BBQR split/join error
    Encode(encode::Error),      // payload validation, from bwk-qr-protocol
    Decode(decode::Error),      // truncated/invalid wire bytes, from bwk-qr-protocol
}
// impl Display + std::error::Error.
```

`bwk-qr-protocol` keeps its own `reader::Error`, `decode::Error` and `encode::Error`.
None of their messages interpolate a payload, so a single `info()` method per enum
yields both the message and a stable numeric code, which is what the C binding hands
back. The messages carry a trailing nul so C gets the very same literal.

---

## 9. Consumer integration

### 9.1 Silent (existing CXX bridge)
C++ never links `bwk-qr` directly. Silent's `silent` crate depends on `bwk-qr`
and re-exposes a thin surface in its `#[cxx::bridge]` (`silent/src/lib.rs`, impl in
`account.rs`), per Silent's FFI rules (no `Result` across the bridge;
sentinel/empty returns). Planned surface (added per phase):
- `struct Image { data: Vec<u8>, width: u32, height: u32 }` + `fn qr_encode(data:
  &str) -> Image`, implemented via `Encoder::encode_text`; the Receive screen blits
  the raster.
- `fn qr_decode_grayscale(gray: &[u8], width: u32, height: u32) -> Vec<String>` for
  the camera scanner, implemented via a `Decoder` (`process` the frame).
- Signing-flow surface: build a `Sign` request -> `Encoder::encode_request` -> frames
  to animate; an opaque `Box<Decoder>` (`process`/`progress`) to
  reassemble the response; then `Account::broadcast_signed_psbt(psbt: Vec<u8>)`.

Each bwk change bumps the pinned `bwk` git `rev` in `silent/Cargo.toml`, refreshes
`Cargo.lock`, and updates `flake.lock` + the `importCargoLock` `outputHashes`.

### 9.2 A C signer
Firmware links `bwk-qr-protocol` (features `ffi`) as an rlib from its own `staticlib`
crate, which supplies the global allocator and the panic handler a library cannot.
`include/bwk_qr_protocol.h` covers the signer direction: decode a request, encode a
response. See `qr-protocol/README.md`.

### 9.3 Other Rust callers
```rust
// plain QR (generation only, zero transitive deps):
use bwk_qr::{Config, Encoder};
let frame = Encoder::new(Config::default())?.encode_text("bc1q...")?;

// protocol:
use bwk_qr::{Config, Encoder};
let frames = Encoder::new(Config::default())?.encode_request(&req)?;
```

---

## 10. Testing

- `gen`/`scan` correctness is exercised via internal (crate-private) tests and
  end-to-end through `Encoder`/`Decoder`; the primitives are not called publicly.
  The plain-string round-trip is `Encoder::encode_text -> Decoder::process`.
  Internally: known-vector sizes; long (~100+ char) address encodes within
  `max_qr_version`; the generated grayscale `Image` feeds the scan helper directly
  and equals input; synthetic grayscale frames (quiet zone + scale).
- `protocol`:
  - round-trip per message (`Encoder::encode_* -> render -> Decoder` yields the
    original);
  - **version compatibility** (internal codec tests): a v1 decoder reads a synthetic
    v(next) blob's known fields and ignores the tail; a v(next) decoder reads a v1
    blob (missing trailing field) correctly;
  - truncated/garbage frame -> `Error::Decode`; malformed descriptor form ->
    `Error::Protocol`;
  - `Encoder::encode_request` yields grayscale frames that feed (ordered +
    shuffled) into a `Decoder` directly;
  - assert the BBQR header (`B$`, generic binary file type `B`).
- Offline, deterministic, plain `cargo test -p bwk-qr`. Build/test matrix:
  `--no-default-features --features gen`, `--features scan`, `--features protocol`,
  `--all-features`. `cargo clippy --all-features` clean.

---

## 11. Risks and open questions

- **Protocol is a draft.** Several fields are our choice; bump the wire `version`
  and only append fields. Add the candidates listed in `ENCODING.md` (spend-path
  selection, multi-descriptor registration) as new versions.
- **Wire-format spec discipline.** The append-only rule only holds if existing
  fields are never reordered or resized; enforce this in review. `qr-protocol/src/lib.rs`
  states the convention at the top: everything there is version 1, and a field added later
  is appended at the end of its body with a `// since vN` marker.
- **External BBQR implementation.** Replace the internal generic-binary subset only
  when a fixed release or pinned fork exposes the `B` file type and safe parsing.
- **`bitcoin` byte serializations.** Use stable forms (BIP-174 PSBT, 78-byte `Xpub`,
  4-byte `Fingerprint`); keep these out of the version-evolving region by treating
  them as opaque `bytes`/fixed fields. The codec models them exactly that way, which
  is what lets it drop the `bitcoin` dependency.
- **UR support** deferred; revisit if a target signer needs `ur:` instead of BBQR.
