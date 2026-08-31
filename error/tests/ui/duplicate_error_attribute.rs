use bwk_error::Error;

#[derive(Debug, Error)]
enum Duplicate {
    #[error("first")]
    #[error("second")]
    BadValue(u32),
}

fn main() {}
