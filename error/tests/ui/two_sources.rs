use bwk_error::Error;

#[derive(Debug, Error)]
enum TwoSources {
    #[error("read {path} failed")]
    Read {
        path: String,
        #[source]
        first: std::io::Error,
        #[source]
        second: std::io::Error,
    },
}

fn main() {}
