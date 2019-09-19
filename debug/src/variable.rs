#[derive(Debug)]
pub struct Variable {
    pub name: String,
    pub file: String,
    pub line: u64,
    pub value: u64,
}