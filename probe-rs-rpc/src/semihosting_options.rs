use std::convert::Infallible;

use postcard_schema::Schema;
use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Schema, Clone)]
pub enum Mapping {
    Exact(String, String),
    Prefix(String, String),
    Regex(String, String),
}

#[derive(Serialize, Deserialize, Schema, Clone, Default)]
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
