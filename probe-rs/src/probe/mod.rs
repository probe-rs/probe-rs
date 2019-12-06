pub mod daplink;
pub mod stlink;

pub mod debug_probe;

#[derive(Copy, Clone, Debug)]
pub enum WireProtocol {
    Swd,
    Jtag,
}
