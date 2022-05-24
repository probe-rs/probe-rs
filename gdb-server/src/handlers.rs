use crate::architecture::{GdbArchitectureExt, GdbTargetExt};
use probe_rs::{Core, CoreStatus, MemoryInterface, Session};
use std::time::Duration;

pub(crate) fn q_supported() -> Option<String> {
    Some(
        "PacketSize=2048;swbreak-;hwbreak+;vContSupported+;qXfer:features:read+;qXfer:memory-map:read+"
            .into(),
    )
}

pub(crate) fn reply_empty() -> Option<String> {
    Some("".into())
}

pub(crate) fn reply_ok() -> Option<String> {
    Some("OK".into())
}

pub(crate) fn q_attached() -> Option<String> {
    Some("1".into())
}

pub(crate) fn halt_reason() -> Option<String> {
    Some("S05".into())
}

pub(crate) fn read_general_registers(mut core: Core) -> Option<String> {
    // First we check the core status.
    // If the core is not properly halted it does not make much sense to try and read registers.
    // On some cores this even leads to a fault!
    match core.status() {
        Err(e) => {
            log::debug!("Unable to read register 0. Reason:");
            log::debug!("{:#?}", e);
            // Tell GDB that we encountered an error reading the register (because of an unhalted core) with a EFAULT response.
            // Errno values can be found here: https://sourceware.org/gdb/current/onlinedocs/gdb/Errno-Values.html
            // More descriptions do not exist.
            return Some("E14".to_string());
        }
        // The core is halted and we can read the register and return its value.
        Ok(CoreStatus::Halted(_)) => (),
        Ok(_) => {
            log::info!("Unable to read register 0 because of a running core.");
            log::info!("Try to halt the core on attach if this problem persists.");
            // Tell GDB that we encountered an error reading the register (because of an unhalted core) with a EFAULT response.
            // Errno values can be found here: https://sourceware.org/gdb/current/onlinedocs/gdb/Errno-Values.html
            // More descriptions do not exist.
            return Some("E14".to_string());
        }
    }

    let mut general_registers_value = String::new();

    for reg in 0..core.num_general_registers() {
        let (probe_rs_number, bytesize) = core.translate_gdb_register_number(reg as u32)?;

        let mut value = core.read_core_reg(probe_rs_number).unwrap();

        for _ in 0..bytesize {
            let byte = value as u8;
            general_registers_value.push_str(&format!("{:02x}", byte));
            value >>= 8;
        }
    }

    Some(general_registers_value)
}

pub(crate) fn read_register(register: u32, mut core: Core) -> Option<String> {
    // First we check the core status.
    // If the core is not properly halted it does not make much sense to try and read registers.
    // On some cores this even leads to a fault!
    match core.status() {
        Err(e) => {
            log::debug!("Unable to read register {}. Reason:", register);
            log::debug!("{:#?}", e);
            // Tell GDB that we encountered an error reading the register (because of an unhalted core) with a EFAULT response.
            // Errno values can be found here: https://sourceware.org/gdb/current/onlinedocs/gdb/Errno-Values.html
            // More descriptions do not exist.
            return Some("E14".to_string());
        }
        // The core is halted and we can read the register and return its value.
        Ok(CoreStatus::Halted(_)) => (),
        Ok(_) => {
            log::info!(
                "Unable to read register {} because of a running core.",
                register
            );
            log::info!("Try to halt the core on attach if this problem persists.");
            // Tell GDB that we encountered an error reading the register (because of an unhalted core) with a EFAULT response.
            // Errno values can be found here: https://sourceware.org/gdb/current/onlinedocs/gdb/Errno-Values.html
            // More descriptions do not exist.
            return Some("E14".to_string());
        }
    }

    let (probe_rs_number, bytesize) = core.translate_gdb_register_number(register)?;

    let mut value = core.read_core_reg(probe_rs_number).unwrap();

    let mut register_value = String::new();

    for _ in 0..bytesize {
        let byte = value as u8;
        register_value.push_str(&format!("{:02x}", byte));
        value >>= 8;
    }

    Some(register_value)
}

pub(crate) fn write_general_registers(reg_values: &str, mut core: Core) -> Option<String> {
    // First we check the core status.
    // If the core is not properly halted it does not make much sense to try and write registers.
    // On some cores this even leads to a fault!
    match core.status() {
        Err(e) => {
            log::debug!("Unable to write register 0. Reason:");
            log::debug!("{:#?}", e);
            // Tell GDB that we encountered an error writing the register (because of an unhalted core) with a EFAULT response.
            // Errno values can be found here: https://sourceware.org/gdb/current/onlinedocs/gdb/Errno-Values.html
            // More descriptions do not exist.
            return Some("E14".to_string());
        }
        // The core is halted and we can read the register and return its value.
        Ok(CoreStatus::Halted(_)) => (),
        Ok(_) => {
            log::info!("Unable to write register 0 because of a running core.");
            log::info!("Try to halt the core on attach if this problem persists.");
            // Tell GDB that we encountered an error writing the register (because of an unhalted core) with a EFAULT response.
            // Errno values can be found here: https://sourceware.org/gdb/current/onlinedocs/gdb/Errno-Values.html
            // More descriptions do not exist.
            return Some("E14".to_string());
        }
    }

    let mut current_str_regval_offset = 0;

    for reg_num in 0..core.num_general_registers() as u32 {
        let (addr, bytesize) = core.translate_gdb_register_number(reg_num)?;

        // TODO: remove, when `Core::write_core_reg()` supports larger registers
        if bytesize as usize > std::mem::size_of::<u32>() {
            // Currently registers larger than 32 bits are not supported
            log::warn!("Register {} is truncated, because probe-rs does not currently support registers longer than 32 bit", reg_num);
        }

        let current_str_regval_end = current_str_regval_offset + bytesize as usize * 2;

        if current_str_regval_end > reg_values.len() {
            // Supplied write general registers command argument length not valid, tell GDB
            log::error!(
                "Unable to write register {}, because supplied register value length was too short",
                reg_num
            );
            return Some("E22".to_string());
        }

        let str_value = &reg_values[current_str_regval_offset..current_str_regval_end];

        let mut value = 0;
        for (exp, ch) in str_value
            .as_bytes()
            .chunks(2)
            .enumerate()
            // TODO: remove, when `Core::write_core_reg()` supports larger registers
            .take(std::mem::size_of::<u32>())
        {
            value +=
                u32::from_str_radix(std::str::from_utf8(ch).unwrap(), 16).unwrap() << (8 * exp);
        }

        core.write_core_reg(addr, value).unwrap();

        current_str_regval_offset = current_str_regval_end;

        if current_str_regval_offset == reg_values.len() {
            break;
        }
    }

    reply_ok()
}

pub(crate) fn write_register(register: u32, hex_value: &str, mut core: Core) -> Option<String> {
    // First we check the core status.
    // If the core is not properly halted it does not make much sense to try and write registers.
    // On some cores this even leads to a fault!
    match core.status() {
        Err(e) => {
            log::debug!("Unable to write register {}. Reason:", register);
            log::debug!("{:#?}", e);
            // Tell GDB that we encountered an error writing the register (because of an unhalted core) with a EFAULT response.
            // Errno values can be found here: https://sourceware.org/gdb/current/onlinedocs/gdb/Errno-Values.html
            // More descriptions do not exist.
            return Some("E14".to_string());
        }
        // The core is halted and we can read the register and return its value.
        Ok(CoreStatus::Halted(_)) => (),
        Ok(_) => {
            log::info!(
                "Unable to write register {} because of a running core.",
                register
            );
            log::info!("Try to halt the core on attach if this problem persists.");
            // Tell GDB that we encountered an error writing the register (because of an unhalted core) with a EFAULT response.
            // Errno values can be found here: https://sourceware.org/gdb/current/onlinedocs/gdb/Errno-Values.html
            // More descriptions do not exist.
            return Some("E14".to_string());
        }
    }

    let (probe_rs_number, bytesize) = core.translate_gdb_register_number(register)?;

    // TODO: remove, when `Core::write_core_reg()` supports larger registers
    if bytesize as usize > std::mem::size_of::<u32>() {
        // Currently registers larger than 32 bits are not supported
        log::warn!("Register {} is truncated, because probe-rs does not currently support registers longer than 32 bit", register);
    }

    let mut value = 0;

    for (exp, ch) in hex_value
        .as_bytes()
        .chunks(2)
        .enumerate()
        // TODO: remove, when `Core::write_core_reg()` supports larger registers
        .take(std::mem::size_of::<u32>())
    {
        value += u32::from_str_radix(std::str::from_utf8(ch).unwrap(), 16).unwrap() << (8 * exp);
    }

    core.write_core_reg(probe_rs_number, value).unwrap();

    reply_ok()
}

pub(crate) fn read_memory(address: u64, length: u32, mut core: Core) -> Option<String> {
    let mut readback_data = vec![0u8; length as usize];
    match core.read(address, &mut readback_data) {
        Ok(_) => Some(
            readback_data
                .iter()
                .map(|s| format!("{:02x?}", s))
                .collect::<Vec<String>>()
                .join(""),
        ),
        // We have no clue if this is the right error code since GDB doesn't feel like docs.
        // We just assume Linux ERRNOs and pick a fitting one: https://gist.github.com/greggyNapalm/2413028#file-gistfile1-txt-L138
        // This seems to work in practice and seems to be the way to do stuff around GDB.
        Err(_e) => Some("E79".to_string()),
    }
}

pub(crate) fn vcont_supported() -> Option<String> {
    // It is important to announce support for both
    // the variants with and without signal support,
    // i.e. both c and C, otherwise GDB will not use
    // the command.
    Some("vCont;c;C;t;s;S".into())
}

pub(crate) fn host_info() -> Option<String> {
    // cputype    12 = arm
    // cpusubtype 14 = v6m
    // See https://llvm.org/doxygen/Support_2MachO_8h_source.html
    Some("cputype:12;cpusubtype:14;triple:armv6m--none-eabi;endian:litte;ptrsize:4".to_string())
}

pub(crate) fn run(mut core: Core, awaits_halt: &mut bool) -> Option<String> {
    core.run().unwrap();
    *awaits_halt = true;
    None
}

pub(crate) fn stop(mut core: Core, awaits_halt: &mut bool) -> Option<String> {
    core.halt(Duration::from_millis(100)).unwrap();
    *awaits_halt = false;
    Some("OK".into())
}

pub(crate) fn step(mut core: Core, awaits_halt: &mut bool) -> Option<String> {
    core.step().unwrap();
    *awaits_halt = false;
    Some("S05".into())
}

pub(crate) fn insert_hardware_break(address: u64, _kind: u32, mut core: Core) -> Option<String> {
    core.set_hw_breakpoint(address).unwrap();
    Some("OK".into())
}

pub(crate) fn remove_hardware_break(address: u64, _kind: u32, mut core: Core) -> Option<String> {
    core.clear_hw_breakpoint(address).unwrap();
    Some("OK".into())
}

pub(crate) fn write_memory(address: u64, data: &[u8], mut core: Core) -> Option<String> {
    core.write_8(address, data).unwrap();

    Some("OK".into())
}

pub(crate) fn get_memory_map(session: &Session) -> Option<String> {
    let memory_map = session.target().gdb_memory_map();

    Some(String::from_utf8(gdb_sanitize_file(memory_map.as_bytes(), 0, 1000)).unwrap())
}

pub(crate) fn user_halt(mut core: Core, awaits_halt: &mut bool) -> Option<String> {
    let _ = core.halt(Duration::from_millis(100));
    *awaits_halt = false;
    Some("T02".into())
}

pub(crate) fn detach(break_due: &mut bool) -> Option<String> {
    *break_due = true;
    Some("OK".into())
}

pub(crate) fn reset_halt(mut core: Core) -> Option<String> {
    let _cpu_info = core.reset_and_halt(Duration::from_millis(400));
    Some("OK".into())
}

fn gdb_sanitize_file(data: &[u8], offset: u32, len: u32) -> Vec<u8> {
    let offset = offset as usize;
    let len = len as usize;
    let mut end = offset + len;
    if offset > data.len() {
        b"l".to_vec()
    } else {
        if end > data.len() {
            end = data.len();
        }
        let mut trimmed_data = Vec::from(&data[offset..end]);
        if trimmed_data.len() >= len {
            // XXX should this be <= or < ?
            trimmed_data.insert(0, b'm');
        } else {
            trimmed_data.insert(0, b'l');
        }
        trimmed_data
    }
}

pub(crate) fn read_target_description(session: &Session, annex: &[u8]) -> Option<String> {
    // Only target.xml is supported
    if annex == b"target.xml" {
        let description = session.target().target_description();

        Some(String::from_utf8(gdb_sanitize_file(description.as_bytes(), 0, 1000)).unwrap())
    } else {
        None
    }
}
