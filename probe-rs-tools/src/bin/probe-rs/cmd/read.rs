use std::io::Write;
use std::path::PathBuf;

use probe_rs::{probe::list::Lister, MemoryInterface};

use crate::util::common_options::{ProbeOptions, ReadWriteBitWidth, ReadWriteOptions};
use crate::CoreOptions;

/// Read from target memory address
///
/// e.g. probe-rs read b32 0x400E1490 2
///      Reads 2 32-bit words from address 0x400E1490
///
/// Output is a space separated list of hex values padded to the read word width.
/// e.g. 2 words
///     00 00 (8-bit)
///     00000000 00000000 (32-bit)
///     0000000000000000 0000000000000000 (64-bit)
///
/// NOTE: Only supports RAM addresses
#[derive(clap::Parser)]
#[clap(verbatim_doc_comment)]
pub struct Cmd {
    #[clap(flatten)]
    shared: CoreOptions,

    #[clap(flatten)]
    probe_options: ProbeOptions,

    #[clap(flatten)]
    read_write_options: ReadWriteOptions,

    /// Number of words to read from the target
    words: u64,

    /// The path to the file to be created with the flash data (iHex format)
    path: Option<PathBuf>,
}

impl Cmd {
    pub fn run(self, lister: &Lister) -> anyhow::Result<()> {
        let (mut session, _probe_options) = self.probe_options.simple_attach(lister)?;

        let mut core = session.core(self.shared.core)?;
        let words = self.words as usize;

        match self.read_write_options.width {
            ReadWriteBitWidth::B8 => {
                let mut values = vec![0; words];
                core.read_8(self.read_write_options.address, &mut values)?;
                if let Some(path) = &self.path {
                    let mut records = vec![];
                    const CHUNK_SIZE: usize = 16;
                    for chunk in values.chunks(CHUNK_SIZE).enumerate() {
                        let address =
                            self.read_write_options.address as usize + chunk.0 * CHUNK_SIZE;
                        records.push((address, chunk.1.to_vec()));
                    }
                    let mut file = std::fs::File::create(path)?;
                    let mut out_records = vec![];
                    // get first record to store starting upper address
                    let (addr, _) = records[0];
                    let mut segment_upper_address = addr >> 16;
                    out_records.push(ihex::Record::StartLinearAddress(
                        segment_upper_address as u32,
                    ));
                    // iterate through records and push them to output vector
                    for (addr, value) in records.into_iter() {
                        let upper = addr >> 16;
                        // write extend linear address record if it has changed
                        if upper != segment_upper_address {
                            out_records.push(ihex::Record::ExtendedLinearAddress(upper as u16));
                        }
                        let offset = addr & 0xffff;
                        out_records.push(ihex::Record::Data {
                            offset: offset as u16,
                            value: value.clone(),
                        });
                        segment_upper_address = upper;
                    }
                    // push EOF record
                    out_records.push(ihex::Record::EndOfFile);
                    let data = ihex::create_object_file_representation(&out_records)?;
                    file.write_all(data.as_bytes())?;
                    println!("File {:?} written.", path);
                } else {
                    for val in values {
                        print!("{:02x} ", val);
                    }
                    println!();
                }
            }
            ReadWriteBitWidth::B32 => {
                if self.path.is_some() {
                    println!("[PATH] only considered with 'b8' WIDTH")
                }
                let mut values = vec![0; words];
                core.read_32(self.read_write_options.address, &mut values)?;
                for val in values {
                    print!("{:08x} ", val);
                }
                println!();
            }
            ReadWriteBitWidth::B64 => {
                if self.path.is_some() {
                    println!("[PATH] only considered with 'b8' WIDTH")
                }
                let mut values = vec![0; words];
                core.read_64(self.read_write_options.address, &mut values)?;
                for val in values {
                    print!("{:016x} ", val);
                }
                println!();
            }
        }
        std::mem::drop(core);

        session.resume_all_cores()?;

        Ok(())
    }
}
