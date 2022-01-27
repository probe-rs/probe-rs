use serde::{Deserialize, Serialize};

/// Data for spesific device
#[derive(Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum DeviceData {
    /// For tinyX devices that uses updi
    AvrTinyX(TinyXDeviceData),
}
#[derive(Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct TinyXDeviceData {
    pub prog_base: u32,
    pub flash_pages_bytes: u16,
    pub eeprom_pages_bytes: u8,
    pub nvmctrl_module_address: u16,
    pub ocd_module_address: u16,
    //_padding: [u8; 10],
    pub flash_bytes: u32,
    pub eeprom_bytes: u16,
    pub user_sig_bytes_bytes: u16,
    pub fuse_bytes: u8,
    //padding: [u8; 5]
    pub eeprom_base: u16,
    pub user_row_base: u16,
    pub sigrow_base: u16,
    pub fuses_base: u16,
    pub lock_base: u16,
    pub device_id: u32,
    //prog_base_msb
    //flash_pages_bytes_msb
    pub address_size: AddressSize,
}

#[derive(Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum AddressSize {
    Size24bit = 0x01,
    Size16bit = 0x00,
}
