use bwk_error::Error;

#[derive(Debug, Error)]
enum Generic<T: std::fmt::Debug> {
    #[error("bad value: {0:?}")]
    BadValue(T),
}

fn main() {}
