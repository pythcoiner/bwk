use bwk_error::Error;

#[derive(Debug, Error)]
enum BareIdent {
    #[error(oops)]
    BadValue(u32),
}

#[derive(Debug, Error)]
enum NotALiteral {
    #[error(concat!("a", "b"))]
    BadValue(u32),
}

#[derive(Debug, Error)]
enum NotAString {
    #[error(42)]
    BadValue(u32),
}

fn main() {}
