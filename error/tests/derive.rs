//! Covers each shape the derive supports.

use bwk_error::Error;
use std::error::Error as _;

const EXPECTED_SIZE: usize = 80;

#[derive(Debug, Error)]
enum Simple {
    #[error("no inputs")]
    NoInputs,
    #[error("bad value: {0}")]
    BadValue(u32),
    #[error("{0} does not match {1}")]
    Mismatch(String, String),
    #[error("debug formatting: {0:?}")]
    Debugged(Vec<u8>),
}

#[derive(Debug, Error)]
enum WithSource {
    #[error("io failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse failed")]
    Parse(#[source] std::num::ParseIntError),
    #[error(transparent)]
    Inner(#[from] Simple),
}

#[derive(Debug, Error)]
enum Named {
    #[error("address {address:?} is invalid on {network}")]
    InvalidAddress { address: String, network: String },
    #[error("wrong size, expected {expected}")]
    WrongSize { expected: usize },
}

#[derive(Debug, Error)]
enum ExtraArgs {
    #[error("header must be {size} bytes", size = EXPECTED_SIZE)]
    BadHeader,
    #[error("got {0}, expected {expected}", expected = EXPECTED_SIZE)]
    Mismatch(usize),
    #[error("{} of {max}", max = EXPECTED_SIZE)]
    ImplicitWithNamedExtra(usize),
}

#[derive(Debug, Error)]
enum FieldNamedF {
    #[error("named {f}")]
    Named { f: u8 },
    #[error("tuple {0}")]
    Tuple(u8),
}

#[derive(Debug, Error)]
#[error("struct {f}")]
struct StructFieldNamedF {
    f: u8,
}

#[test]
fn unit_and_tuple_variants_render() {
    assert_eq!(Simple::NoInputs.to_string(), "no inputs");
    assert_eq!(Simple::BadValue(7).to_string(), "bad value: 7");
    assert_eq!(
        Simple::Mismatch("a".into(), "b".into()).to_string(),
        "a does not match b"
    );
    assert_eq!(
        Simple::Debugged(vec![1, 2]).to_string(),
        "debug formatting: [1, 2]"
    );
}

#[test]
fn named_fields_render_by_name() {
    let e = Named::InvalidAddress {
        address: "bc1".into(),
        network: "signet".into(),
    };
    assert_eq!(e.to_string(), "address \"bc1\" is invalid on signet");
    assert_eq!(
        Named::WrongSize { expected: 4 }.to_string(),
        "wrong size, expected 4"
    );
}

#[test]
fn extra_format_arguments_are_passed_through() {
    assert_eq!(ExtraArgs::BadHeader.to_string(), "header must be 80 bytes");
    assert_eq!(ExtraArgs::Mismatch(3).to_string(), "got 3, expected 80");
}

#[test]
fn an_implicit_slot_takes_the_field_when_every_extra_is_named() {
    assert_eq!(ExtraArgs::ImplicitWithNamedExtra(3).to_string(), "3 of 80");
}

#[test]
fn a_field_named_f_does_not_shadow_the_formatter() {
    assert_eq!(FieldNamedF::Named { f: 7 }.to_string(), "named 7");
    assert_eq!(FieldNamedF::Tuple(7).to_string(), "tuple 7");
    assert_eq!(StructFieldNamedF { f: 7 }.to_string(), "struct 7");
}

#[test]
fn from_generates_the_conversion() {
    let e: WithSource = std::io::Error::other("boom").into();
    assert_eq!(e.to_string(), "io failed: boom");
    assert!(matches!(e, WithSource::Io(_)));
}

#[test]
fn source_is_exposed_for_from_and_source_fields() {
    let e: WithSource = std::io::Error::other("boom").into();
    assert_eq!(e.source().unwrap().to_string(), "boom");

    let inner = "x".parse::<u32>().unwrap_err();
    let e = WithSource::Parse(inner);
    assert_eq!(e.to_string(), "parse failed");
    assert!(e.source().is_some());
}

#[test]
fn variants_without_a_source_report_none() {
    assert!(Simple::NoInputs.source().is_none());
    assert!(Simple::BadValue(1).source().is_none());
    assert!(Named::WrongSize { expected: 1 }.source().is_none());
}

#[test]
fn transparent_defers_display_to_the_inner_error() {
    let e = WithSource::Inner(Simple::BadValue(9));
    // Display is the inner error's, with no wrapper text of its own.
    assert_eq!(e.to_string(), "bad value: 9");
    // The inner error has no source, so neither does the wrapper.
    assert!(e.source().is_none());
}

#[test]
fn errors_work_as_boxed_trait_objects() {
    fn boxed() -> Result<(), Box<dyn std::error::Error>> {
        Err(Simple::NoInputs)?
    }
    assert_eq!(boxed().unwrap_err().to_string(), "no inputs");
}

#[derive(Debug, Error)]
#[error("{0}")]
struct NewtypeError(String);

#[derive(Debug, Error)]
enum BoxedSource {
    #[error("boxed: {0}")]
    Boxed(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("with an explicit argument {0} of {:?}", LIMITS)]
    Explicit(u8),
}

const LIMITS: [u8; 2] = [1, 2];

#[test]
fn struct_errors_render_and_are_errors() {
    let e = NewtypeError("bad config".into());
    assert_eq!(e.to_string(), "bad config");
    let _: &dyn std::error::Error = &e;
}

#[test]
fn boxed_trait_object_sources_are_exposed() {
    let inner: Box<dyn std::error::Error + Send + Sync> = Box::new(Simple::NoInputs);
    let e = BoxedSource::Boxed(inner);
    assert_eq!(e.to_string(), "boxed: no inputs");
    assert_eq!(e.source().unwrap().to_string(), "no inputs");
}

#[derive(Debug, Error)]
enum NamedSource {
    #[error("decode failed at {index}: {source}")]
    Decode {
        index: usize,
        source: std::num::ParseIntError,
    },
    #[error("wrapped {source} while reading {path}")]
    AttributeWins {
        path: String,
        source: String,
        #[source]
        cause: std::io::Error,
    },
}

#[derive(Debug, Error)]
enum Outer {
    #[error("outer")]
    Inner(#[source] WithSource),
}

#[derive(Debug, Error)]
enum Wrapper {
    #[error(transparent)]
    Sourced(#[from] WithSource),
}

#[test]
fn a_field_named_source_is_the_source_without_an_attribute() {
    let inner = "x".parse::<u32>().unwrap_err();
    let e = NamedSource::Decode {
        index: 3,
        source: inner,
    };
    assert_eq!(
        e.to_string(),
        "decode failed at 3: invalid digit found in string"
    );
    assert_eq!(
        e.source().unwrap().to_string(),
        "invalid digit found in string"
    );
}

#[test]
fn an_explicit_attribute_beats_a_field_merely_named_source() {
    let e = NamedSource::AttributeWins {
        path: "/tmp/x".into(),
        source: "some text".into(),
        cause: std::io::Error::other("boom"),
    };
    assert_eq!(e.to_string(), "wrapped some text while reading /tmp/x");
    assert_eq!(e.source().unwrap().to_string(), "boom");
}

#[test]
fn a_source_chain_walks_two_levels_down() {
    let e = Outer::Inner(WithSource::Io(std::io::Error::other("boom")));
    let first = e.source().unwrap();
    assert_eq!(first.to_string(), "io failed: boom");
    assert_eq!(first.source().unwrap().to_string(), "boom");
}

#[test]
fn transparent_reports_the_inner_errors_own_source() {
    let e = Wrapper::Sourced(WithSource::Io(std::io::Error::other("boom")));
    assert_eq!(e.to_string(), "io failed: boom");
    // The wrapper is skipped: the chain goes straight to the io error.
    let source = e.source().unwrap();
    assert_eq!(source.to_string(), "boom");
    assert!(source.source().is_none());
}

#[test]
fn explicit_arguments_fill_implicit_slots_while_numbered_refer_to_fields() {
    assert_eq!(
        BoxedSource::Explicit(9).to_string(),
        "with an explicit argument 9 of [1, 2]"
    );
}

#[derive(Debug, Error)]
enum Braces {
    #[error("literal {{braces}} and {0}")]
    Around(u8),
    #[error("{{{0}}}")]
    Hugging(u8),
    #[error("{{0}}")]
    NotAPlaceholder,
}

#[derive(Debug, Error)]
enum Spec {
    #[error("[{0:>8}]")]
    Width(u32),
    #[error("{0:.3}")]
    Precision(f64),
}

#[derive(Debug, Error)]
enum FromNamedField {
    #[error("io failed: {source}")]
    Io {
        #[from]
        source: std::io::Error,
    },
}

#[derive(Debug, Error)]
#[error("the wallet is locked")]
struct UnitStruct;

#[derive(Debug, Error)]
#[error("{code}: {message}")]
struct MultiFieldStruct {
    code: u32,
    message: String,
}

#[test]
fn escaped_braces_render_as_literal_braces() {
    assert_eq!(Braces::Around(7).to_string(), "literal {braces} and 7");
    assert_eq!(Braces::Hugging(7).to_string(), "{7}");
    assert_eq!(Braces::NotAPlaceholder.to_string(), "{0}");
}

#[test]
fn width_and_precision_specs_are_kept() {
    assert_eq!(Spec::Width(7).to_string(), "[       7]");
    assert_eq!(Spec::Precision(1.234_56).to_string(), "1.235");
}

#[test]
fn from_generates_the_conversion_for_a_named_field() {
    let e: FromNamedField = std::io::Error::other("boom").into();
    assert_eq!(e.to_string(), "io failed: boom");
    assert_eq!(e.source().unwrap().to_string(), "boom");
}

#[test]
fn unit_and_multi_field_structs_render() {
    assert_eq!(UnitStruct.to_string(), "the wallet is locked");
    let e = MultiFieldStruct {
        code: 3,
        message: "no such file".into(),
    };
    assert_eq!(e.to_string(), "3: no such file");
    assert!(e.source().is_none());
}
