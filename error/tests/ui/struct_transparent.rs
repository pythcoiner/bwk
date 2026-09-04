use bwk_error::Error;

#[derive(Debug, Error)]
#[error(transparent)]
struct StructTransparent(std::io::Error);

fn main() {}
