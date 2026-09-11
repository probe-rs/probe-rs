//! Times reading a list of unrelated addresses one at a time against reading them as a batch.
//!
//! The addresses are deliberately not contiguous, so `read_32` does not apply and the choice is
//! between `read_word_32` in a loop and `ArmMemoryInterface::access_words_32`.

use anyhow::{Context, Result};
use clap::Parser;
use probe_rs::architecture::arm::{FullyQualifiedApAddress, memory::Access32};
use probe_rs::probe::{WireProtocol, list::Lister};
use probe_rs::{MemoryInterface, Permissions, config::TargetSelector};
use std::time::{Duration, Instant};

#[derive(clap::Parser)]
struct Cli {
    #[clap(long)]
    chip: String,
    /// Hexadecimal, with or without an `0x` prefix.
    #[clap(long, default_value = "0x20000000")]
    base: String,
    #[clap(long, default_value = "40")]
    count: usize,
    /// Index into `probe-rs list`. Needed only when more than one probe is attached.
    #[clap(long)]
    probe: Option<usize>,
    #[clap(long)]
    speed: Option<u32>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let base = u64::from_str_radix(cli.base.trim_start_matches("0x"), 16)?;

    let lister = Lister::new();
    let probes = lister.list_all();
    let selected = match cli.probe {
        Some(index) => probes.get(index).context("no probe at that index")?,
        None => {
            if probes.len() > 1 {
                for (index, probe) in probes.iter().enumerate() {
                    println!("[{index}]: {probe}");
                }
                anyhow::bail!("more than one probe attached, pass --probe");
            }
            probes.first().context("no probe")?
        }
    };
    let mut probe = selected.open()?;
    probe.select_protocol(WireProtocol::Swd)?;
    if let Some(speed) = cli.speed {
        probe.set_speed(speed)?;
    }

    let mut session = probe.attach(
        TargetSelector::Unspecified(cli.chip.clone()),
        Permissions::default(),
    )?;

    // Spread the addresses across the region so no two land in one auto-increment window.
    let addresses: Vec<u64> = (0..cli.count).map(|i| base + (i as u64) * 64).collect();

    // Both arms read with the core halted, so the values are stable and comparable.
    let mut core = session.core(0)?;
    core.halt(Duration::from_millis(200))?;

    let start = Instant::now();
    let mut one_at_a_time = Vec::with_capacity(addresses.len());
    for &address in &addresses {
        one_at_a_time.push(core.read_word_32(address)?);
    }
    let loop_time = start.elapsed();
    drop(core);

    // `access_words_32` is on `ArmMemoryInterface`, below the architecture-agnostic `Core`, so
    // reach it through the ARM interface directly.
    let ap = FullyQualifiedApAddress::v1_with_default_dp(0);
    let mut memory = session.get_arm_interface()?.memory_interface(&ap)?;

    let accesses: Vec<Access32> = addresses.iter().map(|&a| Access32::Read(a)).collect();
    let mut batched = vec![0u32; addresses.len()];
    let start = Instant::now();
    memory.access_words_32(&accesses, &mut batched)?;
    let batch_time = start.elapsed();
    drop(memory);

    // Leave the target running. Halting and walking away makes the next person wonder why their
    // board is dead.
    session.core(0)?.run()?;

    let count = addresses.len() as u32;
    println!("addresses:     {count}");
    println!(
        "one at a time: {loop_time:>12.3?} ({:.3?} each)",
        loop_time / count
    );
    println!(
        "batched:       {batch_time:>12.3?} ({:.3?} each)",
        batch_time / count
    );
    println!(
        "values match:  {}",
        if one_at_a_time == batched {
            "yes"
        } else {
            "NO"
        }
    );

    Ok(())
}
