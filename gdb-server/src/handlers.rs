use probe_rs::{Core, MemoryInterface};
use recap::Recap;
use serde::Deserialize;

pub(crate) fn q_supported() -> Option<String> {
    Some("PacketSize=2048;swbreak-;hwbreak+;vContSupported+;qXfer:memory-map:read+".into())
}

pub(crate) fn reply_empty() -> Option<String> {
    Some("".into())
}

pub(crate) fn q_attached() -> Option<String> {
    Some("1".into())
}

pub(crate) fn halt_reason() -> Option<String> {
    Some("S05".into())
}

pub(crate) fn read_general_registers() -> Option<String> {
    Some("xxxxxxxx".into())
}

pub(crate) fn read_register(packet_string: String, core: &mut Core) -> Option<String> {
    #[derive(Debug, Deserialize, PartialEq, Recap)]
    #[recap(regex = r#"p(?P<reg>\w+)"#)]
    struct P {
        reg: String,
    }

    let p = packet_string.parse::<P>().unwrap();

    let _ = core.halt();
    core.wait_for_core_halted().unwrap();

    let value = core
        .read_core_reg(u16::from_str_radix(&p.reg, 16).unwrap())
        .unwrap();

    format!(
        "{}{}{}{}",
        value as u8,
        (value >> 8) as u8,
        (value >> 16) as u8,
        (value >> 24) as u8
    );

    Some(format!(
        "{:02x}{:02x}{:02x}{:02x}",
        value as u8,
        (value >> 8) as u8,
        (value >> 16) as u8,
        (value >> 24) as u8
    ))
}

pub(crate) fn read_memory(packet_string: String, core: &mut Core) -> Option<String> {
    #[derive(Debug, Deserialize, PartialEq, Recap)]
    #[recap(regex = r#"m(?P<addr>\w+),(?P<length>\w+)"#)]
    struct M {
        addr: String,
        length: String,
    }

    let m = packet_string.parse::<M>().unwrap();

    let mut readback_data = vec![0u8; usize::from_str_radix(&m.length, 16).unwrap()];
    match core.read_8(
        u32::from_str_radix(&m.addr, 16).unwrap(),
        &mut readback_data,
    ) {
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
    Some("vCont;c;t;s".into())
}

pub(crate) fn run(core: &mut Core, awaits_halt: &mut bool) -> Option<String> {
    core.run().unwrap();
    *awaits_halt = true;
    None
}

pub(crate) fn stop(core: &mut Core, awaits_halt: &mut bool) -> Option<String> {
    core.halt().unwrap();
    core.wait_for_core_halted().unwrap();
    *awaits_halt = false;
    Some("OK".into())
}

pub(crate) fn step(core: &mut Core, awaits_halt: &mut bool) -> Option<String> {
    core.step().unwrap();
    *awaits_halt = false;
    Some("S05".into())
}

pub(crate) fn insert_hardware_break(packet_string: String, core: &mut Core) -> Option<String> {
    #[derive(Debug, Deserialize, PartialEq, Recap)]
    #[recap(regex = r#"Z1,(?P<addr>\w+),(?P<size>\w+)"#)]
    struct Z1 {
        addr: String,
        size: String,
    }

    let z1 = packet_string.parse::<Z1>().unwrap();

    let addr = u32::from_str_radix(&z1.addr, 16).unwrap();

    core.reset_and_halt().unwrap();
    core.wait_for_core_halted().unwrap();
    core.set_hw_breakpoint(addr).unwrap();
    core.run().unwrap();
    Some("OK".into())
}

pub(crate) fn remove_hardware_break(packet_string: String, core: &mut Core) -> Option<String> {
    #[derive(Debug, Deserialize, PartialEq, Recap)]
    #[recap(regex = r#"z1,(?P<addr>\w+),(?P<size>\w+)"#)]
    struct Z1 {
        addr: String,
        size: String,
    }

    let z1 = packet_string.parse::<Z1>().unwrap();

    let addr = u32::from_str_radix(&z1.addr, 16).unwrap();

    core.reset_and_halt().unwrap();
    core.wait_for_core_halted().unwrap();
    core.clear_hw_breakpoint(addr).unwrap();
    core.run().unwrap();
    Some("OK".into())
}

pub(crate) fn write_memory(packet_string: String, data: &[u8], core: &mut Core) -> Option<String> {
    #[derive(Debug, Deserialize, PartialEq, Recap)]
    #[recap(regex = r#"X(?P<addr>\w+),(?P<length>\w+):(?P<data>[01]*)"#)]
    struct X {
        addr: String,
        length: String,
        data: String,
    }

    let x = packet_string.parse::<X>().unwrap();

    let length = usize::from_str_radix(&x.length, 16).unwrap();
    let data = &data[data.len() - length..];

    core.write_8(u32::from_str_radix(&x.addr, 16).unwrap(), data)
        .unwrap();

    Some("OK".into())
}

pub(crate) fn get_memory_map() -> Option<String> {
    let xml = r#"<?xml version="1.0"?>
<!DOCTYPE memory-map PUBLIC "+//IDN gnu.org//DTD GDB Memory Map V1.0//EN" "http://sourceware.org/gdb/gdb-memory-map.dtd">
<memory-map>
<memory type="ram" start="0x20000000" length="0x4000"/>
<memory type="rom" start="0x00000000" length="0x40000"/>
</memory-map>"#;
    Some(String::from_utf8(gdb_sanitize_file(xml.as_bytes(), 0, 1000)).unwrap())
}

pub(crate) fn user_halt(core: &mut Core, awaits_halt: &mut bool) -> Option<String> {
    let _ = core.halt();
    core.wait_for_core_halted().unwrap();
    *awaits_halt = false;
    Some("T02".into())
}

pub(crate) fn detach(break_due: &mut bool) -> Option<String> {
    *break_due = true;
    Some("OK".into())
}

pub(crate) fn reset_halt(core: &mut Core) -> Option<String> {
    let _cpu_info = core.reset();
    let _cpu_info = core.halt();
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
