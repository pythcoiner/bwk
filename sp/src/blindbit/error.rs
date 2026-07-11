//! Source: adapted from cygnet3/spdk. See `sp/NOTICE`.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("HTTP GET failed: {0}")]
    HttpGet(String),
    #[error("failed to read response body: {0}")]
    ResponseBody(String),
    #[error(transparent)]
    Serialization(#[from] serde_json::Error),
}

impl From<Error> for crate::receiver::error::Error {
    fn from(e: Error) -> Self {
        crate::receiver::error::Error::Backend(Box::new(e))
    }
}
