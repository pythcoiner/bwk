use bwk_error::Error;

#[derive(Debug, Error)]
#[error("io failed")]
struct StructFrom(#[from] std::io::Error);

fn main() {}
