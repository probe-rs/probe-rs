#[derive(Debug, Clone)]
pub enum Error {
    SectionNotFound(&'static str),
    IoError(String),
    Unsupported(String),
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error::IoError(std::error::Error::description(&error).to_owned())
    }
}

impl std::error::Error for Error {}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Error::SectionNotFound(section) => write!(
                f,
                "Section '{}' not found, which is required to be present.",
                section
            ),
            Error::IoError(description) => {
                write!(f, "I/O error while parsing data: {}", description)
            }
            Error::Unsupported(description) => write!(
                f,
                "Unsupported configuration in pdsc found: {}",
                description
            ),
        }
    }
}
