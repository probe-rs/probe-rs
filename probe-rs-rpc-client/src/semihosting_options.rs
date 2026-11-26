use std::convert::Infallible;

use regex::Regex;

/// A mapping from the file names of the target to the resources of the host. This never goes to a
/// server.
#[derive(Clone, Debug)]
pub enum Mapping {
    Exact(String, String),
    Prefix(String, String),
    Regex(String, String),
}

#[derive(Clone, Debug, Default)]
pub struct SemihostingOptions {
    mappings: Vec<Mapping>,
}

impl SemihostingOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mappings(&self) -> &[Mapping] {
        &self.mappings
    }

    pub fn add_file(&mut self, from: String, to: String) -> Result<(), Infallible> {
        self.mappings.push(Mapping::Exact(from, to));
        Ok(())
    }

    pub fn add_file_prefix(&mut self, from: String, to: String) -> Result<(), Infallible> {
        self.mappings.push(Mapping::Prefix(from, to));
        Ok(())
    }

    pub fn add_file_regex(&mut self, re: String, to: String) -> Result<(), regex::Error> {
        let _ = Regex::new(&re)?;
        self.mappings.push(Mapping::Regex(re, to));
        Ok(())
    }
}
