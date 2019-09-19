#[derive(Debug)]
pub(crate) struct Variable {
    pub(crate) name: String,
    pub(crate) file: String,
    pub(crate) line: u64,
    pub(crate) value: u64,
}