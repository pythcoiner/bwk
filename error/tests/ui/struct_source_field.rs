use bwk_error::Error;

#[derive(Debug, Error)]
#[error("io failed")]
struct StructSourceField {
    source: std::io::Error,
}

fn main() {}
