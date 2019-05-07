///! The DAP Access module
/// 
/// 
 
pub trait DAPAccess {
    type Error: std::fmt::Debug;

    /// Reads the DAP register on the specified port and address
    fn read_register(&mut self, port: u16, addr: u16) -> Result<u32, Self::Error>;

    fn read_register_ap_tmp(&mut self, port: u16, addr: u16) -> Result<u32, Self::Error>;

    /// Writes a value to the DAP register on the specified port and address
    fn write_register(&mut self, port: u16, addr: u16, value: u32) -> Result<(), Self::Error>;

    fn write_register_ap_tmp(&mut self, port: u16, addr: u16, value: u32) -> Result<(), Self::Error>;
}