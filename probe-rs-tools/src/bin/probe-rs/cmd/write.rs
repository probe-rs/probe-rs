use probe_rs_rpc_client::RpcClient;

use crate::CoreOptions;
use crate::util::common_options::{ProbeOptions, ReadWriteBitWidth, ReadWriteOptions};
use crate::util::{cli, parse_u64};

/// Write to target memory address
///
/// e.g. probe-rs write b32 0x400E1490 0xDEADBEEF 0xCAFEF00D
///      Writes 0xDEADBEEF to address 0x400E1490 and 0xCAFEF00D to address 0x400E1494
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

    /// Values to write to the target.
    /// Takes a list of integer values and can be specified in decimal (16), hexadecimal (0x10) or octal (0o20) format.
    #[clap(value_parser = parse_u64)]
    values: Vec<u64>,
}

fn ensure_data_in_range(data: &[u64], width: ReadWriteBitWidth) -> anyhow::Result<()> {
    let max = match width {
        ReadWriteBitWidth::B8 => u8::MAX as u64,
        ReadWriteBitWidth::B16 => u16::MAX as u64,
        ReadWriteBitWidth::B32 => u32::MAX as u64,
        ReadWriteBitWidth::B64 => u64::MAX,
    };
    if let Some(big) = data.iter().find(|&&v| v > max) {
        anyhow::bail!(
            "{} in {:?} is too large for an {} bit write.",
            big,
            data,
            width as u8,
        );
    }

    Ok(())
}

impl Cmd {
    pub async fn run(self, client: RpcClient) -> anyhow::Result<()> {
        ensure_data_in_range(&self.values, self.read_write_options.width)?;

        let session = cli::attach_probe(&client, self.probe_options, None, false).await?;
        let core = session.core(self.shared.core);

        match self.read_write_options.width {
            ReadWriteBitWidth::B8 => {
                core.write_memory_8(
                    self.read_write_options.address,
                    self.values.iter().map(|v| *v as u8).collect(),
                )
                .await?;
            }
            ReadWriteBitWidth::B16 => {
                core.write_memory_16(
                    self.read_write_options.address,
                    self.values.iter().map(|v| *v as u16).collect(),
                )
                .await?;
            }
            ReadWriteBitWidth::B32 => {
                core.write_memory_32(
                    self.read_write_options.address,
                    self.values.iter().map(|v| *v as u32).collect(),
                )
                .await?;
            }
            ReadWriteBitWidth::B64 => {
                core.write_memory_64(self.read_write_options.address, self.values)
                    .await?;
            }
        }

        // BUG FOUND (2026-09-10): unlike `read.rs` (its `run()` explicitly calls this before
        // returning), this command had no explicit resume at all - it relied entirely on
        // `Session::drop`'s own implicit "resume if halted" teardown. Root-caused on the ARM7TDMI
        // backend via a trace-level comparison: a plain `write` and a plain `read` produce a
        // byte-for-byte identical `resume()` sequence up through the final EmbeddedICE
        // `DebugControl` write and "Core resumed" - `read`'s trace then shows two more
        // `DebugStatus` polls (from this call's own `core_halted()` pre-check, plus
        // `Session::drop`'s own subsequent check) that `write`'s trace never has, since nothing
        // called `resume_all_cores` for it at all. Without this call, a plain `write` reliably
        // left the core in a state where it could never again durably re-enter debug state on the
        // very next attach (`Error: Core is not halted`, needing `probe-rs reset` to recover) -
        // reproduced with the pre-existing, completely unmodified `write_memory_32` path, so this
        // was never an ARM7-specific bug, just this command never resuming the core the same way
        // every other memory-access command does.
        session.resume_all_cores().await?;

        Ok(())
    }
}
