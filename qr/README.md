# bwk-qr

`bwk-qr` provides QR generation, scanning, and the draft signing-flow
message transport for the bwk workspace.

Features:

- `gen` uses `qrcodegen` to render grayscale QR images.
- `scan` uses `quircs` to scan grayscale frames.
- `protocol` adds BBQR generic-binary framing over the signing-flow codec, which
  lives in `bwk-qr-protocol`.

The Rust API returns `Result` and `Option`. FFI consumers should translate those
types at their own boundary; `bwk-qr-protocol` ships a C binding for the codec.
