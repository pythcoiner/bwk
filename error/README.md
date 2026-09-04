# bwk-error

**Experimental. Do not use in production or with real coins. API will break.**

A minimal in-house replacement for `thiserror`'s `#[derive(Error)]`.

It generates the `Display` and `std::error::Error` impls (plus the `From` impl
for `#[from]` fields) for the error shapes this workspace actually writes, so
bwk keeps the usual derive without the dependency.

**Scope:** the `Display`/`Error`/`From` derive only. Does NOT provide an error
type, a `Result` alias, backtraces, or anything resembling `anyhow`.

## Why not upstream `thiserror`

Upstream `thiserror` is a dependency that comes off at a low code cost: the
derive here is a few hundred lines and covers every error shape bwk writes. The
crates it builds on, `syn`, `quote` and `proc-macro2`, are already in the tree
through other proc-macro dependencies, and `trybuild` is a dev-dependency of
this crate only, so it never reaches a consumer's graph. The gain is one fewer
third-party crate in bwk's own dependency list, not a smaller dependency tree
overall.

## The `thiserror` name

The workspace aliases this crate as `thiserror` in `[workspace.dependencies]`:

```toml
thiserror = { package = "bwk-error", path = "error" }
```

The alias is deliberate, not an oversight. Error definitions across the tree
keep reading `#[derive(thiserror::Error)]`, so the switch is a one-line change
in the workspace manifest rather than a rename touching every error enum, which
keeps the diff and the review surface small.

`thiserror::Error` therefore resolves to this crate everywhere in bwk. Upstream
`thiserror` is not a direct dependency of the workspace and nothing here
resolves to it, so anything it supports that is not listed below is simply
unavailable. In particular:

- `#[backtrace]`, and the `Error::provide` machinery around it.
- generic and lifetime parameters on the error type.
- unions.
- more than one `#[from]` or `#[source]` per variant, so no source-plus-backtrace
  variant.
- `#[from]` on a variant with more than one field.
- `#[from]`, `#[source]`, a `source` field or `#[error(transparent)]` on a
  struct.
- `#[source]` on a variant that is already `#[error(transparent)]`.
- the `#[error(fmt = ...)]` form.
- `#[error]`, `#[from]` or `#[source]` in a position the derive does not read.

Each of those is a compile error, never a silent no-op. See "Rejected" below.

## Usage

```rust
#[derive(Debug)]
pub struct Header;

impl Header {
    pub const SIZE: usize = 80;
}

#[derive(Debug, thiserror::Error)]
#[error("decode failed at byte {offset}")]
pub struct DecodeError {
    pub offset: usize,
}

#[derive(Debug, thiserror::Error)]
#[error("the wallet is locked")]
pub struct LockedError;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no inputs")]
    NoInputs,
    #[error("output {0} is not spendable")]
    NotSpendable(u32),
    #[error("header must be {size} bytes", size = Header::SIZE)]
    BadHeader,
    #[error("io failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse failed")]
    Parse(#[source] std::num::ParseIntError),
    #[error("decode failed at index {index}: {source}")]
    Decode {
        index: usize,
        source: DecodeError,
    },
    #[error(transparent)]
    Locked(#[from] LockedError),
}
```

## Supported on enums

- `#[error("...")]` on a variant. Named fields resolve as inline captures and
  tuple fields resolve positionally, so both `{name}` and `{0}` work.
- `#[error("...", extra = EXPR)]` to pass further arguments to the format, either
  positional or named. With a positional argument present, `{}` consumes one of
  those rather than a field; named arguments leave `{}` on the fields.
- `#[error(transparent)]` to defer `Display` to the single inner field and
  `source` to that field's own source.
- `#[from]` on a field, which marks it as the source and generates the `From`
  impl.
- `#[source]` on a field, which only marks it as the source.
- a field named `source` with no attribute, which is taken as the source. An
  explicit `#[from]` or `#[source]` anywhere in the variant wins over it.

A source may be a concrete error type or a boxed `dyn Error`, optionally
`Send`/`Sync`.

## Supported on structs

`#[error("...")]` only. That gives a `Display` impl and an `std::error::Error`
impl with no source.

## Rejected

Each of these is a compile error pointing at the offending item, not a silent
no-op:

- `#[from]` or `#[source]` on a struct or one of its fields. Use an enum if the
  error needs a source.
- a field named `source` on a struct.
- `#[error(transparent)]` on a struct.
- generic or lifetime-parameterised error types.
- unions.
- more than one `#[from]` or `#[source]` in a single variant.
- `#[from]` on a variant with more than one field: the generated `From` has only
  that field to build from.
- `#[error(transparent)]` on a variant that does not have exactly one field.
- `#[error(transparent)]` on a variant that also marks a `#[source]` field: a
  transparent variant defers to the inner error, so there is nothing left to
  point a source at.
- a missing `#[error]` attribute.
- two `#[error]` attributes on the same item.
- an `#[error]` in a position the derive does not read: on an enum itself, or on
  a field. It belongs on each variant of an enum, or on a struct.
- a `#[from]` or a `#[source]` in a position the derive does not read: on an
  enum itself, or on a variant. Either belongs on a field of a variant.
- an `#[error(...)]` whose first argument is not a string literal, such as
  `#[error(oops)]`, `#[error(42)]` or the upstream `#[error(fmt = ...)]` form.
- an unterminated format placeholder, such as `#[error("bad {0")]`.

`#[backtrace]` is not implemented.

That list is the whole rejection surface: `tests/ui/` holds a compile-fail
fixture for every entry, checked by `tests/ui.rs` through `trybuild`. The check
runs on stable only, since the recorded `.stderr` snapshots are the diagnostics
stable renders.
