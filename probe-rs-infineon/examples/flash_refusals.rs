//! PRD test T-12: the TC27x flash sequence refuses invalid requests before the loader runs a
//! flash command. It starts the loader, asks for an unaligned erase, an erase and a program
//! outside the PFLASH, and stops the loader again. No flash is changed.
//!
//! ```text
//! cargo run -p probe-rs-infineon --example flash_refusals
//! ```

use probe_rs::{Permissions, flashing::DebugFlashSequence, probe::list::Lister};
use probe_rs_infineon::flash::Tc27xFlash;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    probe_rs_infineon::register_plugin();
    let probe = Lister::new().open("058b:0000".parse::<probe_rs::probe::DebugProbeSelector>()?)?;
    let mut session = probe.attach("TC275", Permissions::default())?;
    let flash = Tc27xFlash::default();
    let mut failed = 0;

    flash.prepare_flash(&mut session)?;
    let cases: [(&str, Result<(), probe_rs::Error>); 4] = [
        (
            "erase inside a sector (0xA021C010)",
            flash.erase_sector(&mut session, 0xA021_C010),
        ),
        (
            "erase after the PFLASH (0xA0400000)",
            flash.erase_sector(&mut session, 0xA040_0000),
        ),
        (
            "erase in DFLASH (0xAF000000)",
            flash.erase_sector(&mut session, 0xAF00_0000),
        ),
        (
            "program after the PFLASH (0x80400000)",
            flash.program(&mut session, 0x8040_0000, &[0; 32]),
        ),
    ];
    for (what, result) in cases {
        let refused = result.is_err();
        println!(
            "[{}] T-12 {what}: {}",
            if refused { "PASS" } else { "FAIL" },
            match result {
                Err(e) => format!("refused: {e}"),
                Ok(()) => "accepted".to_string(),
            }
        );
        failed += (!refused) as usize;
    }
    flash.finish_flash(&mut session)?;
    drop(session);

    println!("{failed} check(s) failed");
    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}
