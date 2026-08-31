use bwk_error::Error;

#[derive(Debug, Error)]
enum TransparentMultiField {
    #[error(transparent)]
    Read(std::io::Error, String),
}

fn main() {}
