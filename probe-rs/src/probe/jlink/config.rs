#[derive(Default, Clone, Copy, Debug)]
#[expect(dead_code)]
pub struct JlinkConfig {
    pub usb_address: Option<u8>,
    pub kickstart_power: Option<bool>,
    pub ip_address: Option<[u8; 4]>,
    pub subnet_mask: Option<[u8; 4]>,
    pub mac_address: Option<[u8; 6]>,
}

impl JlinkConfig {
    pub fn parse(data: [u8; 256]) -> Result<Self, String> {
        let usb_address = match data[0] {
            0 => Some(0),
            1 => Some(1),
            2 => Some(2),
            0xFF => None,
            other => return Err(format!("Unexpected USB address configured: {other}")),
        };

        let kickstart_power = match u32::from_le_bytes([data[4], data[5], data[6], data[7]]) {
            0 => Some(false),
            1 => Some(true),
            u32::MAX => None,
            other => return Err(format!("Unexpected kickstart power value: {other:#010x}")),
        };

        let ip_address = match data[32..36] {
            [0xFF, 0xFF, 0xFF, 0xFF] => None,
            [a, b, c, d] => Some([a, b, c, d]),
            _ => unreachable!(),
        };

        let subnet_mask = match data[36..40] {
            [0xFF, 0xFF, 0xFF, 0xFF] => None,
            [a, b, c, d] => Some([a, b, c, d]),
            _ => unreachable!(),
        };

        let mac_address = match data[48..54] {
            [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF] => None,
            [a, b, c, d, e, f] => Some([a, b, c, d, e, f]),
            _ => unreachable!(),
        };

        Ok(Self {
            usb_address,
            kickstart_power,
            ip_address,
            subnet_mask,
            mac_address,
        })
    }
}
