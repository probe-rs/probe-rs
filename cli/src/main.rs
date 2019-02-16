use std::time::Instant;

use coresight::dap_access::DAPAccess;
use coresight::access_port::AccessPortError;
use coresight::memory_interface::MemoryInterface;
use probe::debug_probe::DebugProbe;

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
}

fn main() {
    let matches = CLI::from_args();

    match matches {
        CLI::List {} => list_connected_devices(),
        CLI::Info { n } => show_info_of_device(n).unwrap(),
        CLI::Reset { n, assert } => reset_target_of_device(n, assert).unwrap(),
        CLI::Dump { n, loc, words } => dump_memory(n, loc, words).unwrap(),
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
                        num, link.info.usb_pid, link.info.version_name
                    );
                });
        }
        Err(e) => {
            println!("{}", e);
        }
    };
}

#[derive(Debug)]
enum Error {
    USB(libusb::Error),
    DeviceNotFound,
    STLinkError(stlink::STLinkError),
    AccessPortError(AccessPortError),
    Custom(&'static str),
}

fn show_info_of_device(n: u8) -> Result<(), Error> {
    let mut context = libusb::Context::new().or_else(|e| {
        println!("Failed to open an USB context.");
        Err(Error::USB(e))
    })?;
    let mut connected_devices = stlink::get_all_plugged_devices(&mut context).or_else(|e| {
        println!("Failed to fetch plugged USB devices.");
        Err(Error::USB(e))
    })?;
    if connected_devices.len() <= n as usize {
        println!("The device with the given number was not found.");
        Err(Error::DeviceNotFound)
    } else {
        Ok(())
    }?;
    let usb_device = connected_devices.remove(n as usize);
    let mut st_link = stlink::STLink::new(usb_device);
    st_link.open().or_else(|e| Err(Error::STLinkError(e)))?;

    let version = st_link
        .get_version()
        .or_else(|e| Err(Error::STLinkError(e)))?;
    let vtg = st_link
        .get_target_voltage()
        .or_else(|e| Err(Error::STLinkError(e)))?;
    println!("Hardware Version: {:?}", version.0);
    println!("JTAG Version: {:?}", version.1);
    println!("Target Voltage: {:?}", vtg);

    st_link
        .attach(probe::protocol::WireProtocol::Swd)
        .or_else(|e| Err(Error::STLinkError(e)))?;
    st_link
        .write_register(0xFFFF, 0x2, 0x2)
        .or_else(|e| Err(Error::STLinkError(e)))?;

    let target_info = st_link
        .read_register(0xFFFF, 0x4)
        .or_else(|e| Err(Error::STLinkError(e)))?;
    let target_info = parse_target_id(target_info);
    println!("Target Identification Register (TARGETID):");
    println!(
        "\tRevision = {}, Part Number = {}, Designer = {}",
        target_info.0, target_info.3, target_info.2
    );

    let target_info = st_link
        .read_register(0xFFFF, 0x0)
        .or_else(|e| Err(Error::STLinkError(e)))?;
    let target_info = parse_target_id(target_info);
    println!("Identification Code Register (IDCODE):");
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
    st_link.close().or_else(|e| Err(Error::STLinkError(e)))?;
    if target_info.3 != 1
        || !(target_info.0 == 0x3 || target_info.0 == 0x4)
        || !(target_info.1 == 0xBA00 || target_info.1 == 0xBA02)
    {
        return Err(Error::Custom(
            "The IDCODE register has not-expected contents.",
        ));
    }
    Ok(())
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
    const CSW_SIZE32: u32 = 0x00000002;
    const CSW_SADDRINC: u32 = 0x00000010;
    const CSW_DBGSTAT: u32 = 0x00000040;
    const CSW_HPROT: u32 = 0x02000000;
    const CSW_MSTRDBG: u32 = 0x20000000;
    const CSW_RESERVED: u32 = 0x01000000;

    const CSW_VALUE: u32 = (CSW_RESERVED | CSW_MSTRDBG | CSW_HPROT | CSW_DBGSTAT | CSW_SADDRINC);

    let mut context = libusb::Context::new().or_else(|e| {
        println!("Failed to open an USB context.");
        Err(Error::USB(e))
    })?;
    let mut connected_devices = stlink::get_all_plugged_devices(&mut context).or_else(|e| {
        println!("Failed to fetch plugged USB devices.");
        Err(Error::USB(e))
    })?;
    if connected_devices.len() <= n as usize {
        println!("The device with the given number was not found.");
        Err(Error::DeviceNotFound)
    } else {
        Ok(())
    }?;
    let usb_device = connected_devices.remove(n as usize);
    let mut st_link = stlink::STLink::new(usb_device);
    st_link.open().or_else(|e| Err(Error::STLinkError(e)))?;

    st_link
        .attach(probe::protocol::WireProtocol::Swd)
        .or_else(|e| Err(Error::STLinkError(e)))?;

    st_link
        .write_register(0x0, 0x0, CSW_VALUE | CSW_SIZE32)
        .ok();

    let mem = MemoryInterface::new(0x0);

    let mut data = vec![0 as u32; words as usize];

    let instant = Instant::now();

    mem.read_block(&mut st_link, loc, &mut data.as_mut_slice()).or_else(|e| Err(Error::AccessPortError(e)))?;

    let elapsed = instant.elapsed();

    for word in 0..words {
        println!("Addr 0x{:08x?}: 0x{:08x}", loc + 4 * word, data[word as usize]);
    }

    println!("Read {:?} words in {:?}", words, elapsed);

    st_link.close().or_else(|e| Err(Error::STLinkError(e)))?;

    Ok(())
}

fn reset_target_of_device(n: u8, assert: Option<bool>) -> Result<(), Error> {
    let mut context = libusb::Context::new().or_else(|e| {
        println!("Failed to open an USB context.");
        Err(Error::USB(e))
    })?;
    let mut connected_devices = stlink::get_all_plugged_devices(&mut context).or_else(|e| {
        println!("Failed to fetch plugged USB devices.");
        Err(Error::USB(e))
    })?;
    if connected_devices.len() <= n as usize {
        println!("The device with the given number was not found.");
        Err(Error::DeviceNotFound)
    } else {
        Ok(())
    }?;
    let usb_device = connected_devices.remove(n as usize);
    let mut st_link = stlink::STLink::new(usb_device);
    st_link.open().or_else(|e| Err(Error::STLinkError(e)))?;

    if let Some(assert) = assert {
        println!(
            "{} target reset.",
            if assert { "Asserting" } else { "Deasserting" }
        );
        st_link
            .drive_nreset(assert)
            .or_else(|e| Err(Error::STLinkError(e)))?;
        println!(
            "Target reset has been {}.",
            if assert { "asserted" } else { "deasserted" }
        );
    } else {
        println!("Triggering target reset.");
        st_link
            .target_reset()
            .or_else(|e| Err(Error::STLinkError(e)))?;
        println!("Target reset has been triggered.");
    }
    st_link.close().or_else(|e| Err(Error::STLinkError(e)))?;
    Ok(())
}
