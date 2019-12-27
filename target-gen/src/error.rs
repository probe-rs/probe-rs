#[derive(Debug, Clone)]
pub enum Error {
    SectionNotFound(&'static str),
    IoError(String),
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error::IoError(std::error::Error::description(&error).to_owned())
    }
}
