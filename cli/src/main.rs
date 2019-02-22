use std::time::Instant;

use coresight::dap_access::DAPAccess;
use coresight::access_port::AccessPortError;
use coresight::memory_interface::MemoryInterface;
use probe::debug_probe::{DebugProbe, DebugProbeError};

use structopt::StructOpt;

fn parse_hex(src: &str) -> Result<u32, std::num::ParseIntError> {
    u32::from_str_radix(src, 16)
}

#[derive(StructOpt)]
#[structopt(
    name = "ST-Link CLI",
    about = "Get info about the connected ST-Links",
    author = "Noah Hüsser <yatekii@yatekii.ch>"
)]
enum CLI {
    /// List all connected ST-Links
    #[structopt(name = "list")]
    List {},
    /// Gets infos about the selected ST-Link
    #[structopt(name = "info")]
    Info {
        /// The number associated with the ST-Link to use
        n: u8,
    },
    /// Resets the target attached to the selected ST-Link
    #[structopt(name = "reset")]
    Reset {
        /// The number associated with the ST-Link to use
        n: u8,
        /// Whether the reset pin should be asserted or deasserted. If left open, just pulse it
        assert: Option<bool>,
    },
    /// Dump memory from attached target
    #[structopt(name = "dump")]
    Dump {
        /// The number associated with the ST-Link to use
        n: u8,
        /// The address of the memory to dump from the target (in hexadecimal without 0x prefix)
        #[structopt(parse(try_from_str = "parse_hex"))]
        loc: u32,
        /// The amount of memory (in words) to dump
        words: u32,
    },
    /// Download memory to attached target
    // #[structopt(name = "download")]
    // Download {
    //     /// The number associated with the ST-Link to use
    //     n: u8,
    //     /// The address of the memory to download to the target (in hexadecimal without 0x prefix)
    //     #[structopt(parse(try_from_str = "parse_hex"))]
    //     loc: u32,
    //     /// The the word to write to memory
    //     #[structopt(parse(try_from_str = "parse_hex"))]
    //     word: u32,
    // },
    #[structopt(name = "trace")]
    Trace {
        /// The number associated with the ST-Link to use
        n: u8,
        /// The address of the memory to dump from the target (in hexadecimal without 0x prefix)
        #[structopt(parse(try_from_str = "parse_hex"))]
        loc: u32,
    },
}

fn main() {
    let matches = CLI::from_args();

    match matches {
        CLI::List {} => list_connected_devices(),
        CLI::Info { n } => show_info_of_device(n).unwrap(),
        CLI::Reset { n, assert } => reset_target_of_device(n, assert).unwrap(),
        CLI::Dump { n, loc, words } => dump_memory(n, loc, words).unwrap(),
        //CLI::Download { n, loc, word } => download(n, loc, word).unwrap(),
        CLI::Trace { n, loc } => trace_u32_on_target(n, loc).unwrap(),
    }
}

fn list_connected_devices() {
    let mut context = libusb::Context::new().unwrap();
    match stlink::get_all_plugged_devices(&mut context) {
        Ok(connected_stlinks) => {
            println!("The following devices were found:");
            connected_stlinks
                .iter()
                .enumerate()
                .for_each(|(num, link)| {
                    println!(
                        "[{}]: PID = {}, version = {}",
                        num, link.1.usb_pid, link.1.version_name
                    );
                });
        }
        Err(e) => {
            println!("{:?}", e);
        }
    };
}

#[derive(Debug)]
enum Error {
    DebugProbeError(DebugProbeError),
    AccessPortError(AccessPortError),
    Custom(&'static str),
    StdIOError(std::io::Error),
}

fn show_info_of_device(n: u8) -> Result<(), Error> {
    with_device(n, |st_link| {
                println!("EKKEKEEK");
        let version = st_link
            .get_version()
            .or_else(|e| Err(Error::DebugProbeError(e)))?;
                println!("EKKEKEEK");
        let vtg = st_link
            .get_target_voltage()
            .or_else(|e| Err(Error::DebugProbeError(e)))?;
                println!("EKKEKEEK");

        println!("Device information:");
        println!("\nHardware Version: {:?}", version.0);
        println!("\nJTAG Version: {:?}", version.1);
        println!("\nTarget Voltage: {:?}", vtg);

        st_link
            .write_register(0xFFFF, 0x2, 0x2)
            .or_else(|e| Err(Error::DebugProbeError(e)))?;

        let target_info = st_link
            .read_register(0xFFFF, 0x4)
            .or_else(|e| Err(Error::DebugProbeError(e)))?;
        let target_info = parse_target_id(target_info);
        println!("\nTarget Identification Register (TARGETID):");
        println!(
            "\tRevision = {}, Part Number = {}, Designer = {}",
            target_info.0, target_info.3, target_info.2
        );

        let target_info = st_link
            .read_register(0xFFFF, 0x0)
            .or_else(|e| Err(Error::DebugProbeError(e)))?;
        let target_info = parse_target_id(target_info);
        println!("\nIdentification Code Register (IDCODE):");
        println!(
            "\tProtocol = {},\n\tPart Number = {},\n\tJEDEC Manufacturer ID = {:x}",
            if target_info.0 == 0x4 {
                "JTAG-DP"
            } else if target_info.0 == 0x3 {
                "SW-DP"
            } else {
                "Unknown Protocol"
            },
            target_info.1,
            target_info.2
        );

        if target_info.3 != 1
            || !(target_info.0 == 0x3 || target_info.0 == 0x4)
            || !(target_info.1 == 0xBA00 || target_info.1 == 0xBA02)
        {
            return Err(Error::Custom(
                "The IDCODE register has not-expected contents.",
            ));
        }
        Ok(())
    })
}

// revision | partno | designer | reserved
// 4 bit    | 16 bit | 11 bit   | 1 bit
fn parse_target_id(value: u32) -> (u8, u16, u16, u8) {
    (
        (value >> 28) as u8,
        (value >> 12) as u16,
        ((value >> 1) & 0x07FF) as u16,
        (value & 0x01) as u8,
    )
}

fn dump_memory(n: u8, loc: u32, words: u32) -> Result<(), Error> {
    with_device(n, |st_link| {
        let mem = MemoryInterface::new(0x0);
        let mut data = vec![0 as u32; words as usize];

        // Start timer.
        let instant = Instant::now();

        mem.read_block(st_link, loc, &mut data.as_mut_slice()).or_else(|e| Err(Error::AccessPortError(e)))?;

        // Stop timer.
        let elapsed = instant.elapsed();

        // Print read values.
        for word in 0..words {
            println!("Addr 0x{:08x?}: 0x{:08x}", loc + 4 * word, data[word as usize]);
        }
        // Print stats.
        println!("Read {:?} words in {:?}", words, elapsed);

        Ok(())
    })
}

// TODO: highly unfinished.
// fn download(n: u8, loc: u32, word: u32) -> Result<(), Error> {
//     const CSW_SIZE32: u32 = 0x00000002;
//     const CSW_SADDRINC: u32 = 0x00000010;
//     const CSW_DBGSTAT: u32 = 0x00000040;
//     const CSW_HPROT: u32 = 0x02000000;
//     const CSW_MSTRDBG: u32 = 0x20000000;
//     const CSW_RESERVED: u32 = 0x01000000;

//     const CSW_VALUE: u32 = (CSW_RESERVED | CSW_MSTRDBG | CSW_HPROT | CSW_DBGSTAT | CSW_SADDRINC);

//     let mut context = libusb::Context::new().or_else(|e| {
//         println!("Failed to open an USB context.");
//         Err(Error::USB(e))
//     })?;
//     let mut connected_devices = stlink::get_all_plugged_devices(&mut context).or_else(|e| {
//         println!("Failed to fetch plugged USB devices.");
//         Err(Error::USB(e))
//     })?;
//     if connected_devices.len() <= n as usize {
//         println!("The device with the given number was not found.");
//         Err(Error::DeviceNotFound)
//     } else {
//         Ok(())
//     }?;
//     let usb_device = STLinkUSBDevice::new(|device| device.remove(n as usize));
//     let mut st_link = stlink::STLink::new(usb_device);
//     st_link.open().or_else(|e| Err(Error::DebugProbeError(e)))?;

//     st_link
//         .attach(probe::protocol::WireProtocol::Swd)
//         .or_else(|e| Err(Error::DebugProbeError(e)))?;

//     st_link
//         .write_register(0x0, 0x0, CSW_VALUE | CSW_SIZE32)
//         .ok();

//     let mem = MemoryInterface::new(0x0);

//     let instant = Instant::now();

//     mem.write(&mut st_link, loc, word).or_else(|e| Err(Error::AccessPortError(e)))?;

//     let elapsed = instant.elapsed();

//     println!("Addr 0x{:08x?}: 0x{:08x}", loc, word);

//     println!("Wrote 1 word in {:?}", elapsed);

//     st_link.close().or_else(|e| Err(Error::DebugProbeError(e)))?;

//     Ok(())
// }

fn reset_target_of_device(n: u8, assert: Option<bool>) -> Result<(), Error> {
    with_device(n, |st_link| {
        if let Some(assert) = assert {
            println!(
                "{} target reset.",
                if assert { "Asserting" } else { "Deasserting" }
            );
            st_link
                .drive_nreset(assert)
                .or_else(|e| Err(Error::DebugProbeError(e)))?;
            println!(
                "Target reset has been {}.",
                if assert { "asserted" } else { "deasserted" }
            );
        } else {
            println!("Triggering target reset.");
            st_link
                .target_reset()
                .or_else(|e| Err(Error::DebugProbeError(e)))?;
            println!("Target reset has been triggered.");
        }
        Ok(())
    })
}

fn trace_u32_on_target(n: u8, loc: u32) -> Result<(), Error> {
    use std::io::prelude::*;
    use std::thread::sleep;
    use std::time::Duration;
    use scroll::{Pwrite};

    let mut xs = vec![];
    let mut ys = vec![];

    let start = Instant::now();
    let mem = MemoryInterface::new(0x0);

    with_device(n, |st_link| {
        loop {
            // Prepare read.
            let elapsed = start.elapsed();
            let instant = elapsed.as_secs() * 1000 + elapsed.subsec_millis() as u64;

            // Read data.
            let value: u32 = mem.read(st_link, loc).or_else(|e| Err(Error::AccessPortError(e)))?;

            xs.push(instant);
            ys.push(value);

            // Send value to plot.py.
            // Unwrap is safe as there is always an stdin in our case!
            let mut buf = [0 as u8; 8];
            // Unwrap is safe!
            buf.pwrite(instant, 0).unwrap();
            buf.pwrite(value, 4).unwrap();
            std::io::stdout().write(&buf).or_else(|e| Err(Error::StdIOError(e)))?;
            std::io::stdout().flush().or_else(|e| Err(Error::StdIOError(e)))?;

            // Schedule next read.
            let elapsed = start.elapsed();
            let instant = elapsed.as_secs() * 1000 + elapsed.subsec_millis() as u64;
            let poll_every_ms = 50;
            let time_to_wait = poll_every_ms - instant % poll_every_ms;
            sleep(Duration::from_millis(time_to_wait));
        }
    })?;

    Ok(())
}

/// Takes a closure that is handed an `STLink` instance and then executed.
/// After the closure is done, the USB device is always closed,
/// even in an error case inside the closure!
fn with_device<F>(n: u8, mut f: F) -> Result<(), Error>
where
    F: FnMut(&mut stlink::STLink) -> Result<(), Error>
{
    let mut st_link = stlink::STLink::new_from_connected(|mut devices| {
        if devices.len() <= n as usize {
            println!("The device with the given number was not found.");
            Err(libusb::Error::NotFound)
        } else {
            Ok(devices.remove(n as usize).0)
        }
    }).or_else(|e| Err(Error::DebugProbeError(e)))?;

    st_link
        .attach(probe::protocol::WireProtocol::Swd)
        .or_else(|e| Err(Error::DebugProbeError(e)))?;
    
    f(&mut st_link)
}
