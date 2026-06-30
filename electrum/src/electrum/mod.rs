pub mod method;
pub mod params;
pub mod request;
pub mod response;
pub mod types;

#[derive(Debug)]
pub enum Error {
    InvalidParam,
    MethodNotFound,
    ResponseParsing(serde_json::Error),
    RawResponseParsing(serde_json::Error),
    ResponseId(usize),
    BatchParsing,
    WrongMethod,
}
