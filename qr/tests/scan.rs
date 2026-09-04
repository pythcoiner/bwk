#![cfg(all(feature = "gen", feature = "scan"))]

use bwk_qr::{Config, Decoded, Decoder, Encoder, Error, Image};

const TAPROOT_ADDRESS: &str = "bc1p5d7rjq7g6rdk2yhzks9smlaqtedr4dekq08ge8ztwac72sfr9rusxg3297";
const SP_ADDRESS: &str = "sp1qqgste7k9hx0qftg6qmwlkqtwuy6cycyavzmzj85c6qdfhjdpdjtdgqjuexzk6murw56suy3e0rd2cgqvycxttddwsvgxe2usfpxumr70xc9pkqwv";
const BIP21_URI: &str =
    "bitcoin:bc1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh?amount=0.0125&label=Coffee";

fn render(text: &str) -> Image {
    let encoder = Encoder::new(Config::default()).unwrap();
    encoder.encode_text(text).unwrap()
}

fn assert_round_trip(text: &str) {
    let mut decoder = Decoder::new(Config::default()).unwrap();
    let decoded = decoder.process(&render(text)).unwrap();
    assert_eq!(decoded, vec![Decoded::Text(text.to_string())]);
}

#[test]
fn taproot_address_round_trips() {
    assert_round_trip(TAPROOT_ADDRESS);
}

#[test]
fn sp_address_round_trips() {
    assert_round_trip(SP_ADDRESS);
}

#[test]
fn mixed_case_text_round_trips() {
    assert_round_trip("Bwk QR Test 12345 aBcDeF");
}

#[test]
fn bip21_uri_round_trips() {
    assert_round_trip(BIP21_URI);
}

#[test]
fn inverted_frame_decodes_only_when_configured() {
    let rendered = render(TAPROOT_ADDRESS);
    let inverted = Image {
        data: rendered.data.iter().map(|v| 255 - *v).collect(),
        width: rendered.width,
        height: rendered.height,
    };

    let mut permissive = Decoder::new(Config::default()).unwrap();
    assert_eq!(
        permissive.process(&inverted).unwrap(),
        vec![Decoded::Text(TAPROOT_ADDRESS.to_string())]
    );

    let mut strict = Decoder::new(Config {
        scan_inverted: false,
        ..Config::default()
    })
    .unwrap();
    assert_eq!(strict.process(&inverted).unwrap(), vec![]);
}

#[test]
fn frame_without_qr_decodes_to_nothing() {
    let mut decoder = Decoder::new(Config::default()).unwrap();
    let gray = Image {
        data: vec![128; 100 * 100],
        width: 100,
        height: 100,
    };
    assert_eq!(decoder.process(&gray).unwrap(), vec![]);
}

#[test]
fn frame_over_the_pixel_limit_is_rejected() {
    let mut decoder = Decoder::new(Config {
        max_image_pixels: 64,
        ..Config::default()
    })
    .unwrap();
    assert_eq!(
        decoder.process(&render(TAPROOT_ADDRESS)),
        Err(Error::BadFrame)
    );
}
