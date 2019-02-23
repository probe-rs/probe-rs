#[macro_use] extern crate rental;

mod usb_interface;
pub mod constants;
mod stlink;

pub use crate::stlink::{
    STLink,
};
pub use crate::usb_interface::{
    STLinkUSBDevice,
    get_all_plugged_devices,
};