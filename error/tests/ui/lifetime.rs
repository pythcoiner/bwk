use bwk_error::Error;

#[derive(Debug, Error)]
enum Borrowed<'a> {
    #[error("bad value: {0}")]
    BadValue(&'a str),
}

fn main() {}
