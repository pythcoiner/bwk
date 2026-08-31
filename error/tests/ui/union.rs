use bwk_error::Error;

#[derive(Error)]
#[error("union")]
union Pair {
    a: u32,
    b: f32,
}

fn main() {}
