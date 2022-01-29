/// Tries to get the register name of a specific address of an architecture.
pub fn try_get_register_name(address: u32) -> Option<String> {
    Some(match address {
        0xE000_EDF0 => "DHCSR".into(),
        0xE000_EDF4 => "DCRSR".into(),
        0xE000_EDF8 => "DCRDR".into(),
        0xE000_2000 => "BP_CTRL".into(),
        0xE000_2008 => "BP_CTRL0".into(),
        addr @ 0xE000_2008..=0xE000_2028 => format!("BP_CTRL{}", (addr - 0xE000_2008) / 4),
        0xE000_ED0C => "AIRCR".into(),
        0xE000_EDFC => "DEMCR".into(),
        _ => return None,
    })
}
