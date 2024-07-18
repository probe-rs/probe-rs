use std::{env::ArgsOs, fs, io::Write, path::Path};

use anyhow::{Context, Error, Result};
use probe_rs_mi::meta::Meta;
use serde::Serialize;
use zip::write::FileOptions;

use crate::util::meta::current_meta;

#[derive(Serialize)]
pub struct Report<'a> {
    pub meta: Meta,
    pub command: Vec<String>,
    pub elf: Option<&'a Path>,
    pub log: Option<&'a Path>,
    #[serde(serialize_with = "serialize_anyhow")]
    pub error: anyhow::Error,
}

impl<'a> Report<'a> {
    pub fn new(
        command: ArgsOs,
        error: anyhow::Error,
        elf: Option<&'a Path>,
        log: Option<&'a Path>,
    ) -> Result<Self> {
        Ok(Self {
            meta: current_meta()?,
            command: command.map(|s| s.to_string_lossy().to_string()).collect(),
            elf,
            log,
            error,
        })
    }

    pub fn zip(&self, path: &Path) -> Result<()> {
        let file = fs::File::create(path)
            .with_context(|| format!("{} could not be opened", path.display()))?;

        let mut archive = zip::ZipWriter::new(file);
        let options = FileOptions::<()>::default();

        archive.start_file("meta.json", options)?;
        serde_json::to_writer_pretty(&mut archive, self)?;

        if let Some(elf) = self.elf {
            archive.start_file("elf.elf", options)?;
            archive.write_all(&fs::read(elf)?)?;
        }

        if let Some(log) = self.log {
            archive.start_file("log.txt", options)?;
            archive.write_all(&fs::read(log)?)?;
        }

        Ok(())
    }
}

pub fn serialize_anyhow<S>(error: &Error, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&format!("{error:?}"))
}
