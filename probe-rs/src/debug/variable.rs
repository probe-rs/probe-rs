use super::*;

#[derive(Debug, Default)]
pub struct Variable {
    pub name: String,
    pub file: String,
    pub line: u64,
    pub value: u64,
    pub typ: Type,
}
