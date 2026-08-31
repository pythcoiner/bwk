use bwk_error::Error;

#[derive(Debug, Error)]
enum FromMultiField {
    #[error("read {path} failed")]
    Read(#[from] std::io::Error, String),
}

fn main() {}
