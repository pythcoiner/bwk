use bwk_error::Error;

#[derive(Debug, Error)]
#[error("on the enum itself")]
enum OnTheEnum {
    #[error("bad value: {0}")]
    BadValue(u32),
}

#[derive(Debug, Error)]
enum OnAVariantField {
    #[error("bad value: {0}")]
    BadValue(#[error("on the field")] u32),
}

#[derive(Debug, Error)]
#[error("bad value: {value}")]
struct OnAStructField {
    #[error("on the field")]
    value: u32,
}

fn main() {}
