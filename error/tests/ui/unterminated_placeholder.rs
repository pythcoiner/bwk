use bwk_error::Error;

#[derive(Debug, Error)]
enum Unterminated {
    #[error("bad {0")]
    BadValue(u32),
}

fn main() {}
