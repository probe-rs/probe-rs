//! Reads the OCDS trigger switch (OTGS) registers, and tries a halt routing between the cores.
//!
//! `dump` prints the trigger switch registers of Cerberus and the debug event registers of
//! CPU0-2. `try <TRC> <EXEVT>` writes `<TRC>` to `CBS_TRC0`-`CBS_TRC2` and `<EXEVT>` to the
//! `EXEVT` of CPU0-2, halts CPU1 with a `DBGSR` write, samples the run state of the cores, runs
//! them again, and puts the registers back. Nothing is written to flash, and a reset clears all
//! of it anyway.
//!
//! ```text
//! cargo run -p probe-rs-infineon --example trigger_switch -- dump
//! cargo run -p probe-rs-infineon --example trigger_switch -- try 0x00700007 0x12
//! ```

use std::time::{Duration, Instant};

use probe_rs::{
    Permissions, Session,
    architecture::tricore::bus::AddressMap,
    probe::{DebugProbeSelector, list::Lister},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const CBS: u32 = 0xF000_0000;
const CBS_REGISTERS: &[(&str, u32)] = &[
    ("TIPR", 0x410),
    ("TOPR", 0x414),
    ("TOPPS", 0x418),
    ("TCIP", 0x41C),
    ("TRC0", 0x420),
    ("TRC1", 0x424),
    ("TRC2", 0x428),
    ("TRC3", 0x42C),
    ("TRHSM", 0x438),
    ("TRMC", 0x43C),
    ("TRSS", 0x460),
    ("TLS", 0x470),
    ("TCTL", 0x474),
    ("OEC", 0x478),
    ("OCNTRL", 0x47C),
    ("OSTATE", 0x480),
    ("TLC", 0x490),
    ("TL1ST", 0x494),
    ("TLCHE", 0x498),
    ("TLCHS", 0x49C),
    ("TRIGS", 0x4A0),
    ("TLT", 0x4A8),
];
const CBS_TRC0: u32 = CBS + 0x420;
const CBS_TLS: u32 = CBS + 0x470;

const CSFR_BASE: u32 = 0xF881_0000;
const CSFR_STRIDE: u32 = 0x2_0000;
const DBGSR: u32 = 0xFD00;
const EXEVT: u32 = 0xFD08;
const CREVT: u32 = 0xFD0C;
const SWEVT: u32 = 0xFD10;
const PC: u32 = 0xFE08;

fn csfr(core: u32, offset: u32) -> u32 {
    CSFR_BASE + core * CSFR_STRIDE + offset
}

fn read(session: &mut Session, address: u32) -> Result<u32> {
    Ok(session
        .get_tricore_bus()?
        .read_u32(AddressMap::Target, address)?)
}

fn write(session: &mut Session, address: u32, value: u32) -> Result<()> {
    Ok(session
        .get_tricore_bus()?
        .write_u32(AddressMap::Target, address, value)?)
}

fn dump(session: &mut Session) -> Result<()> {
    for &(name, offset) in CBS_REGISTERS {
        match read(session, CBS + offset) {
            Ok(value) => println!("CBS_{name:<7} {:#010x}  {value:#010x}", CBS + offset),
            Err(error) => println!("CBS_{name:<7} {:#010x}  ({error})", CBS + offset),
        }
    }
    for core in 0..3 {
        let [dbgsr, exevt, crevt, swevt, pc] =
            [DBGSR, EXEVT, CREVT, SWEVT, PC].map(|offset| read(session, csfr(core, offset)));
        println!(
            "CPU{core}: DBGSR {:#06x}  EXEVT {:#06x}  CREVT {:#06x}  SWEVT {:#06x}  PC {:#010x}",
            dbgsr?, exevt?, crevt?, swevt?, pc?
        );
    }
    Ok(())
}

/// Prints the run state of CPU0-2 and `CBS_TLS` whenever they change, for `time`.
fn sample(session: &mut Session, what: &str, time: Duration) -> Result<()> {
    println!("-- {what}");
    let start = Instant::now();
    let mut last = [u32::MAX; 4];
    while start.elapsed() < time {
        let mut now = [0; 4];
        for (core, value) in now.iter_mut().take(3).enumerate() {
            *value = read(session, csfr(core as u32, DBGSR))?;
        }
        now[3] = read(session, CBS_TLS)?;
        if now != last {
            let state = |v: u32| if (v >> 1) & 3 == 1 { "H" } else { "r" };
            println!(
                "{:>5} ms  CPU0 {} {:#06x}  CPU1 {} {:#06x}  CPU2 {} {:#06x}  TLS {:#04x}",
                start.elapsed().as_millis(),
                state(now[0]),
                now[0],
                state(now[1]),
                now[1],
                state(now[2]),
                now[2],
                now[3]
            );
            last = now;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn run_all(session: &mut Session) {
    for core in 0..3 {
        if let Err(error) = session.core(core).and_then(|mut c| c.run()) {
            println!("run CPU{core}: {error}");
        }
    }
}

fn try_routing(session: &mut Session, trc: u32, exevt: u32) -> Result<()> {
    let saved_trc: Vec<u32> = (0..3)
        .map(|core| read(session, CBS_TRC0 + 4 * core))
        .collect::<Result<_>>()?;
    let saved_exevt: Vec<u32> = (0..3)
        .map(|core| read(session, csfr(core, EXEVT)))
        .collect::<Result<_>>()?;
    println!("saved TRC0-2 {saved_trc:#x?}, EXEVT0-2 {saved_exevt:#x?}");

    let result = (|| -> Result<()> {
        for core in 0..3 {
            write(session, csfr(core, EXEVT), exevt)?;
            write(session, CBS_TRC0 + 4 * core, trc)?;
        }
        let readback: Vec<u32> = (0..3)
            .map(|core| read(session, CBS_TRC0 + 4 * core))
            .collect::<Result<_>>()?;
        println!("TRC0-2 now {readback:#x?}");
        sample(session, "routing set", Duration::from_millis(200))?;

        write(session, csfr(1, DBGSR), 0x6)?;
        sample(
            session,
            "after a halt request on CPU1",
            Duration::from_millis(500),
        )?;

        session.core(1)?.run()?;
        sample(session, "after running CPU1", Duration::from_millis(300))?;
        run_all(session);
        sample(session, "after running CPU0-2", Duration::from_millis(500))?;

        write(session, csfr(1, DBGSR), 0x6)?;
        sample(
            session,
            "after a second halt request on CPU1",
            Duration::from_millis(300),
        )?;
        run_all(session);
        sample(
            session,
            "after running CPU0-2 again",
            Duration::from_millis(300),
        )?;
        Ok(())
    })();

    for core in 0..3 {
        write(session, CBS_TRC0 + 4 * core, saved_trc[core as usize])?;
        write(session, csfr(core, EXEVT), saved_exevt[core as usize])?;
    }
    run_all(session);
    sample(session, "registers put back", Duration::from_millis(200))?;
    result
}

fn parse(value: &str) -> Result<u32> {
    Ok(u32::from_str_radix(value.trim_start_matches("0x"), 16)?)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    probe_rs_infineon::register_plugin();
    let selector: DebugProbeSelector = "058b:0000".parse()?;
    let mut session = Lister::new()
        .open(selector)?
        .attach("TC275", Permissions::default())?;
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["dump"] | [] => dump(&mut session),
        ["reset-halt"] => {
            session
                .core(0)?
                .reset_and_halt(Duration::from_millis(500))?;
            dump(&mut session)
        }
        ["try", trc, exevt] => try_routing(&mut session, parse(trc)?, parse(exevt)?),
        _ => Err("usage: trigger_switch dump | reset-halt | try <TRC> <EXEVT>".into()),
    }
}
