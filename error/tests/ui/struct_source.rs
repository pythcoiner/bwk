use bwk_error::Error;

#[derive(Debug, Error)]
#[error("io failed")]
struct StructSource(#[source] std::io::Error);

fn main() {}
