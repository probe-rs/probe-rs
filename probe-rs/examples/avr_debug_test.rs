//! Quick test of AVR UPDI debug operations.
//! Run with: cargo run --example avr_debug_test
//!
//! Requires an ATmega4809 Curiosity Nano with a blinky flashed.

use probe_rs::config::TargetSelector;
use probe_rs::probe::WireProtocol;
use probe_rs::probe::list::Lister;
use probe_rs::{Permissions, RegisterId};
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let lister = Lister::new();
    let probes = lister.list_all();
    println!("Found {} probes", probes.len());

    let probe_info = probes
        .iter()
        .find(|p| p.vendor_id == 0x03eb && p.product_id == 0x2175)
        .expect("No EDBG probe found");
    println!("Using probe: {probe_info}");

    let mut probe = probe_info.open()?;
    probe.select_protocol(WireProtocol::Updi)?;
    probe.attach_to_unspecified()?;

    let mut session = probe.attach(TargetSelector::Auto, Permissions::default())?;
    println!("Session: {:?}", session.architecture());

    let mut core = session.core(0)?;
    println!("Core: {:?}", core.core_type());

    println!("\n=== Status ===");
    match core.status() {
        Ok(s) => println!("  {s:?}"),
        Err(e) => println!("  ERR: {e}"),
    }

    println!("\n=== Halt ===");
    match core.halt(Duration::from_secs(2)) {
        Ok(info) => println!("  PC = {:#010x}", info.pc),
        Err(e) => println!("  ERR: {e}"),
    }

    println!("\n=== Read PC ===");
    let pc: u32 = match core.read_core_reg(RegisterId(34)) {
        Ok(v) => {
            println!("  PC = {v:#010x}");
            v
        }
        Err(e) => {
            println!("  ERR: {e}");
            0
        }
    };

    println!("\n=== Read R0-R5 ===");
    for i in 0u16..6 {
        let v: u32 = match core.read_core_reg(RegisterId(i)) {
            Ok(v) => {
                println!("  R{i} = {v:#04x}");
                v
            }
            Err(e) => {
                println!("  R{i} ERR: {e}");
                0
            }
        };
        let _ = v;
    }

    println!("\n=== Read SREG ===");
    let _: u32 = match core.read_core_reg(RegisterId(32)) {
        Ok(v) => {
            println!("  SREG = {v:#04x}");
            v
        }
        Err(e) => {
            println!("  ERR: {e}");
            0
        }
    };

    println!("\n=== Read SP ===");
    let _: u32 = match core.read_core_reg(RegisterId(33)) {
        Ok(v) => {
            println!("  SP = {v:#06x}");
            v
        }
        Err(e) => {
            println!("  ERR: {e}");
            0
        }
    };

    println!("\n=== Step ===");
    match core.step() {
        Ok(info) => println!("  PC = {:#010x}", info.pc),
        Err(e) => println!("  ERR: {e}"),
    }

    println!("\n=== Resume ===");
    match core.run() {
        Ok(()) => println!("  OK"),
        Err(e) => println!("  ERR: {e}"),
    }

    println!("\n=== Status after resume ===");
    match core.status() {
        Ok(s) => println!("  {s:?}"),
        Err(e) => println!("  ERR: {e}"),
    }

    let _ = pc;
    println!("\nDone!");
    Ok(())
}
