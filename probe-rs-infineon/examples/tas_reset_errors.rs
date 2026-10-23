//! How a TAS server reports a device reset to its other clients: client A is connected, client
//! B resets the device (`DEVICE_CONNECT` with reset and halt, as `probe-rs reset` does), then A
//! reads `SCU_CHIPID` and prints the status of every frame with the time since the reset.
//!
//! The server refuses the first request of each client after a reset with `DEV_RESET` (0x44);
//! the probe counts it and sends the frame again, so the frames show `ok` and the count tells.
//!
//! `--wait <ms>`: A makes its first access only this long after the reset.
//! `--app`: B requests an application (software) reset instead, through `SCU_SWRSTCON`.
//!
//! The device is left reset; run `probe-rs reset` afterwards to run the application again.
//!
//! ```text
//! cargo run -p probe-rs-infineon --example tas_reset_errors -- [--wait 2000] [--app]
//! ```

use std::time::{Duration, Instant};

use probe_rs::{
    architecture::tricore::bus::{
        AddressMap, BusTransfer, ResetKind, TransferStatus, TriCoreBusAccess,
    },
    probe::DebugProbe,
};
use probe_rs_infineon::tas::{Endpoint, TasProbe};

const SCU_CHIPID: u32 = 0xF003_6140;
const WDTSCON0: u32 = 0xF003_60F0;
const SWRSTCON: u32 = 0xF003_6060;

fn status(probe: &mut TasProbe) -> String {
    let mut transfers = [BusTransfer::read(AddressMap::Target, SCU_CHIPID, 4)];
    match probe.execute(&mut transfers) {
        Err(error) => format!("error {error}"),
        Ok(()) => match transfers[0].status {
            Some(TransferStatus::Ok) => "ok".to_string(),
            Some(TransferStatus::Failed(code)) => format!("{code:#04x}"),
            None => "not executed".to_string(),
        },
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().collect::<Vec<_>>();
    let wait = args
        .iter()
        .position(|a| a == "--wait")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let app = args.iter().any(|a| a == "--app");

    let mut a = TasProbe::new(Endpoint::default());
    a.attach()?;
    println!("A before the reset: {}", status(&mut a));
    let mut b = TasProbe::new(Endpoint::default());
    b.attach()?;

    let reset = Instant::now();
    if app {
        // Safety ENDINIT off through the Safety watchdog, then SWRSTREQ.
        let value = b.read_u32(AddressMap::Target, WDTSCON0)?;
        let password = (value ^ 0xFC) & 0xFFFC;
        let reload = value & 0xFFFF_0000;
        if value & 2 != 0 {
            b.write_u32(AddressMap::Target, WDTSCON0, reload | password | 1)?;
        }
        b.write_u32(AddressMap::Target, WDTSCON0, reload | password | 2)?;
        let _ = b.write_u32(AddressMap::Target, SWRSTCON, 1 << 1);
        println!("B requested an application reset");
    } else {
        b.reset(ResetKind::ResetAndHalt)?;
        println!(
            "B reset the device ({} ms), B then: {}",
            reset.elapsed().as_millis(),
            status(&mut b)
        );
    }

    std::thread::sleep(Duration::from_millis(wait));
    let mut last = String::new();
    let mut count = 0;
    let end = Instant::now() + Duration::from_secs(3);
    while Instant::now() < end {
        let s = status(&mut a);
        if s != last {
            if count > 0 {
                println!("    ... {count} frame(s) {last}");
            }
            println!("A at {:5} ms: {s}", reset.elapsed().as_millis());
            last = s;
            count = 0;
        }
        count += 1;
        std::thread::sleep(Duration::from_millis(5));
    }
    println!("    ... {count} frame(s) {last}");
    println!("B at the end: {}", status(&mut b));
    println!(
        "device resets the server reported (frames sent again): A {}, B {}",
        a.device_resets(),
        b.device_resets()
    );
    Ok(())
}
