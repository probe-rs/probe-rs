pub mod consts {
    // MEM-AP register addresses
    pub const MEM_AP_CSW: u32 = 0x00;
    pub const MEM_AP_TAR: u32 = 0x04;
    pub const MEM_AP_DRW: u32 = 0x0C;

    // Common AP register addresses
    pub const AP_BASE: u32 = 0xF8;
    pub const AP_IDR: u32 = 0xFC;
    pub const APSEL_SHIFT: u32 = 24;

    // AP IDR bitfields:
    // [31:28] Revision
    // [27:24] JEP106 continuation (0x4 for ARM)
    // [23:17] JEP106 vendor ID (0x3B for ARM)
    // [16:13] Class (0b1000=Mem-AP)
    // [12:8]  Reserved
    // [7:4]   AP Variant (non-zero for JTAG-AP)
    // [3:0]   AP Type
    pub const AP_IDR_REVISION_MASK: u32 = 0xf0000000;
    pub const AP_IDR_REVISION_SHIFT: u8 = 28;
    pub const AP_IDR_JEP106_MASK: u32 = 0x0ffe0000;
    pub const AP_IDR_JEP106_SHIFT: u8 = 17;
    pub const AP_IDR_CLASS_MASK: u32 = 0x0001e000;
    pub const AP_IDR_CLASS_SHIFT: u8 = 13;
    pub const AP_IDR_VARIANT_MASK: u32 = 0x000000f0;
    pub const AP_IDR_VARIANT_SHIFT: u8 = 4;
    pub const AP_IDR_TYPE_MASK: u32 = 0x0000000f;

    // MEM-AP type constants
    pub const AP_TYPE_AHB: u8 = 0x1;
    pub const AP_TYPE_APB: u8 = 0x2;
    pub const AP_TYPE_AXI: u8 = 0x4;
    pub const AP_TYPE_AHB5: u8 = 0x5;


    // AP classes
    pub const AP_CLASS_NONE: u8 = 0x00000; // No class defined
    pub const AP_CLASS_MEM_AP: u8 = 0x8; // MEM-AP

    // AP Control and Status Word definitions
    pub const CSW_SIZE: u32 =  0x00000007;
    pub const CSW_SIZE8: u32 =  0x00000000;
    pub const CSW_SIZE16: u32 =  0x00000001;
    pub const CSW_SIZE32: u32 =  0x00000002;
    pub const CSW_ADDRINC: u32 = 0x00000030;
    pub const CSW_NADDRINC: u32 = 0x00000000;
    pub const CSW_SADDRINC: u32 = 0x00000010;
    pub const CSW_PADDRINC: u32 = 0x00000020;
    pub const CSW_DBGSTAT: u32 = 0x00000040;
    pub const CSW_TINPROG: u32 = 0x00000080;
    pub const CSW_HPROT: u32 = 0x02000000;
    pub const CSW_MSTRTYPE: u32 = 0x20000000;
    pub const CSW_MSTRCORE: u32 = 0x00000000;
    pub const CSW_MSTRDBG: u32 = 0x20000000;
    pub const CSW_RESERVED: u32 = 0x01000000;

    pub const CSW_VALUE: u32 = (CSW_RESERVED | CSW_MSTRDBG | CSW_HPROT | CSW_DBGSTAT | CSW_SADDRINC) as u32;
}

pub type AccessPortNumber = u16;

#[derive(Debug)]
pub enum AccessPortError {
    ProbeError,
    InvalidAccessPortNumber,
    MemoryNotAligned,
}

// pub trait AccessPort {
//     fn get_access_port(&self) -> AccessPortNumber;
//     fn set_access_port(&mut self, access_port: AccessPortNumber);

//     fn read_reg(&self, debug_port: &mut DebugPort, addr: u32) -> Result<u32, DebugPortError> {
//         debug_port.read_ap(((self.get_access_port() as u32) << consts::APSEL_SHIFT) | addr)
//     }

//     fn write_reg(&self, debug_port: &mut DebugPort, addr: u32, data: u32) -> Result<(), DebugPortError> {
//         debug_port.write_ap(((self.get_access_port() as u32) << consts::APSEL_SHIFT) | addr, data)
//     }
// }

// /// Determine if an AP exists with the given AP number.
// pub fn access_port_is_valid(debug_port: &mut DebugPort, access_port: AccessPortNumber) -> Result<bool, DebugPortError> {
//     let idr = debug_port.read_ap(((access_port as u32) << consts::APSEL_SHIFT) | consts::AP_IDR as u32)?;
//     Ok(idr != 0)
// }

// /// Determines the type of the AP by examining the IDR value and creates a new
// /// AP object of the appropriate class. See #AP_TYPE_MAP for the mapping of IDR
// /// fields to class.
// pub fn create(debug_port: &mut DebugPort, access_port: AccessPortNumber) -> Result<AccessPortContainer, AccessPortError>  {
//     // Attempt to read the IDR for this APSEL. If we get a zero back then there is
//     // no AP present, so we return None.
//     let idr = debug_port.read_ap(((access_port as u32) << consts::APSEL_SHIFT) | consts::AP_IDR as u32)
//                         .map_err(|e| AccessPortError::from(e))?;
//     if idr == 0 {
//         return Err(AccessPortError::InvalidAccessPortNumber);
//     }
    
//     // Extract IDR fields used for lookup.
//     let designer = (idr & consts::AP_IDR_JEP106_MASK) >> consts::AP_IDR_JEP106_SHIFT;
//     let ap_class = (idr & consts::AP_IDR_CLASS_MASK) >> consts::AP_IDR_CLASS_SHIFT;
//     let variant = (idr & consts::AP_IDR_VARIANT_MASK) >> consts::AP_IDR_VARIANT_SHIFT;
//     let ap_type = (idr & consts::AP_IDR_TYPE_MASK) as u8;

//     // Get the AccessPort class to instantiate.        
//     return match (variant, ap_type) {
//         (0, consts::AP_TYPE_AHB) => Ok(MemoryAccessPort::new(access_port)),
//         (0, consts::AP_TYPE_AHB) => Ok(MemoryAccessPort::new(access_port)),
//         (0, consts::AP_TYPE_AHB) => Ok(MemoryAccessPort::new(access_port)),
//         (0, consts::AP_TYPE_AHB) => Ok(MemoryAccessPort::new(access_port)),
//         (0, consts::AP_TYPE_AHB) => Ok(MemoryAccessPort::new(access_port)),
//         (0, consts::AP_TYPE_APB) => Ok(MemoryAccessPort::new(access_port)),
//         (0, consts::AP_TYPE_AXI) => Ok(MemoryAccessPort::new(access_port)),
//         (0, consts::AP_TYPE_AHB5) => Ok(MemoryAccessPort::new(access_port)),
//     }
// }