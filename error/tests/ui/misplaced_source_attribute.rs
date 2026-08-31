use bwk_error::Error;

#[derive(Debug, Error)]
#[from]
enum OnTheEnum {
    #[error("bad value: {0}")]
    BadValue(u32),
}

#[derive(Debug, Error)]
enum FromOnAVariant {
    #[error("io failed")]
    #[from]
    Io(std::io::Error),
}

#[derive(Debug, Error)]
enum SourceOnAVariant {
    #[error("io failed")]
    #[source]
    Io(std::io::Error),
}

#[derive(Debug, Error)]
#[error("io failed")]
#[source]
struct OnTheStruct(std::io::Error);

fn main() {}
