mod usb_interface;
pub mod constants;
mod stlink;

pub use crate::stlink::{
    STLink,
    STLinkError,
};
pub use crate::usb_interface::{
    STLinkUSBDevice,
    get_all_plugged_devices,
};

// impl DebugProbe for STLink {
        
//     APSEL = 0xff000000
//     APSEL_SHIFT = 24
    
//     @classmethod
//     def get_all_connected_probes(cls):
//         try:
//             return [cls(dev) for dev in usb.STLinkUSBInterface.get_all_connected_devices()]
//         except STLinkException as exc:
//             six.raise_from(cls._convert_exception(exc), exc)
    
//     @classmethod
//     def get_probe_with_id(cls, unique_id):
//         try:
//             for dev in usb.STLinkUSBInterface.get_all_connected_devices():
//                 if dev.serial_number == unique_id:
//                     return cls(usb.STLinkUSBInterface(unique_id))
//             else:
//                 return None
//         except STLinkException as exc:
//             six.raise_from(cls._convert_exception(exc), exc)

//     def __init__(self, device):
//         self._link = stlink.STLink(device)
//         self._is_open = False
//         self._is_connected = False
//         self._nreset_state = False
//         self._memory_interfaces = {}
        
//     @property
//     def description(self):
//         return self.product_name
    
//     @property
//     def vendor_name(self):
//         return self._link.vendor_name
    
//     @property
//     def product_name(self):
//         return self._link.product_name

//     ## @brief Only valid after opening.
//     @property
//     def supported_wire_protocols(self):
//         return [DebugProbe.Protocol.DEFAULT, DebugProbe.Protocol.SWD, DebugProbe.Protocol.JTAG]

//     @property
//     def unique_id(self):
//         return self._link.serial_number

//     @property
//     def wire_protocol(self):
//         return DebugProbe.Protocol.SWD if self._is_connected else None
    
//     @property
//     def is_open(self):
//         return self._is_open
    
//     def open(self):
//         try:
//             self._link.open()
//             self._is_open = True
//         except STLinkException as exc:
//             six.raise_from(self._convert_exception(exc), exc)
    
//     def close(self):
//         try:
//             self._link.close()
//             self._is_open = False
//         except STLinkException as exc:
//             six.raise_from(self._convert_exception(exc), exc)

//     # ------------------------------------------- #
//     #          Target control functions
//     # ------------------------------------------- #
//     def connect(self, protocol=None):
//         """Initialize DAP IO pins for JTAG or SWD"""
//         try:
//             self._link.enter_debug(stlink.STLink.Protocol.SWD)
//             self._is_connected = True
//         except STLinkException as exc:
//             six.raise_from(self._convert_exception(exc), exc)

//     # TODO remove
//     def swj_sequence(self):
//         """Send sequence to activate JTAG or SWD on the target"""
//         pass

//     def disconnect(self):
//         """Deinitialize the DAP I/O pins"""
//         try:
//             # TODO Close the APs. When this is attempted, we get an undocumented 0x1d error. Doesn't
//             #      seem to be necessary, anyway.
//             self._memory_interfaces = {}
            
//             self._link.enter_idle()
//             self._is_connected = False
//         except STLinkException as exc:
//             six.raise_from(self._convert_exception(exc), exc)

//     def set_clock(self, frequency):
//         """Set the frequency for JTAG and SWD in Hz
//         This function is safe to call before connect is called.
//         """
//         try:
//             self._link.set_swd_frequency(frequency)
//         except STLinkException as exc:
//             six.raise_from(self._convert_exception(exc), exc)

//     def reset(self):
//         """Reset the target"""
//         try:
//             self._link.target_reset()
//         except STLinkException as exc:
//             six.raise_from(self._convert_exception(exc), exc)

//     def assert_reset(self, asserted):
//         """Assert or de-assert target reset line"""
//         try:
//             self._link.drive_nreset(asserted)
//             self._nreset_state = asserted
//         except STLinkException as exc:
//             six.raise_from(self._convert_exception(exc), exc)
    
//     def is_reset_asserted(self):
//         """Returns True if the target reset line is asserted or False if de-asserted"""
//         return self._nreset_state

//     def flush(self):
//         """Write out all unsent commands"""
//         pass

//     # ------------------------------------------- #
//     #          DAP Access functions
//     # ------------------------------------------- #
    
//     def read_dp(self, addr, now=True):
//         try:
//             result = self._link.read_dap_register(stlink.STLink.DP_PORT, addr)
//         except STLinkException as exc:
//             six.raise_from(self._convert_exception(exc), exc)
        
//         def read_dp_result_callback():
//             return result
        
//         return result if now else read_dp_result_callback

//     def write_dp(self, addr, data):
//         try:
//             result = self._link.write_dap_register(stlink.STLink.DP_PORT, addr, data)
//         except STLinkException as exc:
//             six.raise_from(self._convert_exception(exc), exc)

//     def read_ap(self, addr, now=True):
//         try:
//             apsel = (addr & self.APSEL) >> self.APSEL_SHIFT
//             result = self._link.read_dap_register(apsel, addr & 0xffff)
//         except STLinkException as exc:
//             six.raise_from(self._convert_exception(exc), exc)
        
//         def read_ap_result_callback():
//             return result
        
//         return result if now else read_ap_result_callback

//     def write_ap(self, addr, data):
//         try:
//             apsel = (addr & self.APSEL) >> self.APSEL_SHIFT
//             result = self._link.write_dap_register(apsel, addr & 0xffff, data)
//         except STLinkException as exc:
//             six.raise_from(self._convert_exception(exc), exc)

//     def read_ap_multiple(self, addr, count=1, now=True):
//         results = [self.read_ap(addr, now=True) for n in range(count)]
        
//         def read_ap_multiple_result_callback():
//             return result
        
//         return results if now else read_ap_multiple_result_callback

//     def write_ap_multiple(self, addr, values):
//         for v in values:
//             self.write_ap(addr, v)

//     def get_memory_interface_for_ap(self, apsel):
//         assert self._is_connected
//         if apsel not in self._memory_interfaces:
//             self._link.open_ap(apsel)
//             self._memory_interfaces[apsel] = STLinkMemoryInterface(self._link, apsel)
//         return self._memory_interfaces[apsel]
  
//     @staticmethod
//     def _convert_exception(exc):
//         if isinstance(exc, STLinkException):
//             return exceptions.ProbeError(str(exc))
//         else:
//             return exc

// ## @brief Concrete memory interface for a single AP.
// class STLinkMemoryInterface(MemoryInterface):
//     def __init__(self, link, apsel):
//         self._link = link
//         self._apsel = apsel

//     ## @brief Write a single memory location.
//     #
//     # By default the transfer size is a word.
//     def write_memory(self, addr, data, transfer_size=32):
//         assert transfer_size in (8, 16, 32)
//         if transfer_size == 32:
//             self._link.write_mem32(addr, conversion.u32le_list_to_byte_list([data]), self._apsel)
//         elif transfer_size == 16:
//             self._link.write_mem16(addr, conversion.u16le_list_to_byte_list([data]), self._apsel)
//         elif transfer_size == 8:
//             self._link.write_mem8(addr, [data], self._apsel)
        
//     ## @brief Read a memory location.
//     #
//     # By default, a word will be read.
//     def read_memory(self, addr, transfer_size=32, now=True):
//         assert transfer_size in (8, 16, 32)
//         if transfer_size == 32:
//             result = conversion.byte_list_to_u32le_list(self._link.read_mem32(addr, 4, self._apsel))[0]
//         elif transfer_size == 16:
//             result = conversion.byte_list_to_u16le_list(self._link.read_mem16(addr, 2, self._apsel))[0]
//         elif transfer_size == 8:
//             result = self._link.read_mem8(addr, 1, self._apsel)[0]
        
//         def read_callback():
//             return result
//         return result if now else read_callback

//     def write_memory_block32(self, addr, data):
//         self._link.write_mem32(addr, conversion.u32le_list_to_byte_list(data), self._apsel)

//     def read_memory_block32(self, addr, size):
// return conversion.byte_list_to_u32le_list(self._link.read_mem32(addr, size * 4, self._apsel))
