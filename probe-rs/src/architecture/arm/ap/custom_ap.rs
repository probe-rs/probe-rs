//! Definition of some custom (proprietary) access ports

use super::{APRegister, AccessPort, GenericAP, Register};

// Ctrl-Ap
// The Control Access PortType (CTRL-AP) is a Nordic's custom access port that enables control of the
// device even if the other access ports in the DAP are being disabled by the access port protection.
define_ap!(CtrlAP);

impl From<GenericAP> for CtrlAP {
    fn from(other: GenericAP) -> Self {
        CtrlAP {
            port_number: other.get_port_number(),
        }
    }
}

define_ap_register!(
    /// Soft reset triggered through CTRL-AP
    CtrlAP,
    RESET,
    0x000,
    [(RESET: bool),],
    value,
    RESET { RESET: value == 1 },
    if value.RESET { 1 } else { 0 }
);

define_ap_register!(
    /// Start mass erase
    CtrlAP,
    ERASEALL,
    0x004,
    [(ERASEALL: bool),],
    value,
    ERASEALL {
        ERASEALL: value == 1,
    },
    if value.ERASEALL { 1 } else { 0 }
);

define_ap_register!(
    /// Flag that indicates if the mass erase process is on-going
    CtrlAP,
    ERASEALLSTATUS,
    0x008,
    [(ERASEALLSTATUS: bool),],
    value,
    ERASEALLSTATUS {
        ERASEALLSTATUS: value == 1,
    },
    if value.ERASEALLSTATUS { 1 } else { 0 }
);

define_ap_register!(
    /// Flag that indicates if the chip is locked, `0` means locked
    CtrlAP,
    APPROTECTSTATUS,
    0x00C,
    [(APPROTECTSTATUS: bool),],
    value,
    APPROTECTSTATUS {
        APPROTECTSTATUS: value == 1,
    },
    if value.APPROTECTSTATUS { 1 } else { 0 }
);
