use serde::{Deserialize, Serialize};
#[derive(Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum DeviceData {
    TinyX(TinyXDeviceData),
}
#[derive(Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct TinyXDeviceData {
    prog_base: u32,
    flash_pages_bytes: u16,
    eeprom_pages_bytes: u8,
    nvmctrl_module_address: u16,
    ocd_module_address: u16,
    //_padding: [u8; 10],
    flash_bytes: u32,
    eeprom_bytes: u16,
    user_sig_bytes_bytes: u16,
    fuse_bytes: u8,
    //padding: [u8; 5]
    eeprom_base: u16,
    user_row_base: u16,
    sigrow_base: u16,
    fuses_base: u16,
    lock_base: u16,
    device_id: u32,
    //prog_base_msb
    //flash_pages_bytes_msb
    address_size: AddressSize,
}

#[derive(Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum AddressSize {
    Size24bit = 0x01,
    Size16bit = 0x00,
}
