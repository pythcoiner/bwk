use bwk_error::Error;

#[derive(Debug, Error)]
enum TransparentWithSource {
    #[error(transparent)]
    Io(#[source] std::io::Error),
}

fn main() {}
