use bwk_error::Error;

#[derive(Debug, Error)]
enum FmtForm {
    #[error(fmt = render_bad_value)]
    BadValue(u32),
}

fn main() {}
