//! Definition of some custom (proprietary) access ports

use crate::coresight::common::Register;

use crate::coresight::access_ports::generic_ap::GenericAP;
use crate::coresight::access_ports::APRegister;
use crate::coresight::ap_access::AccessPort;

// Ctrl-Ap
// The Control Access Port (CTRL-AP) is a Nordic's custom access port that enables control of the
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
    RESET {
        RESET: if value == 1 { true } else { false },
    },
    if value.RESET { 1 } else { 0 }
);

define_ap_register!(
    /// Soft reset triggered through CTRL-AP
    CtrlAP,
    ERASEALL,
    0x004,
    [(ERASEALL: bool),],
    value,
    ERASEALL {
        ERASEALL: if value == 1 { true } else { false },
    },
    if value.ERASEALL { 1 } else { 0 }
);

define_ap_register!(
    /// Soft reset triggered through CTRL-AP
    CtrlAP,
    ERASEALLSTATUS,
    0x008,
    [(ERASEALLSTATUS: bool),],
    value,
    ERASEALLSTATUS {
        ERASEALLSTATUS: if value == 1 { true } else { false },
    },
    if value.ERASEALLSTATUS { 1 } else { 0 }
);

define_ap_register!(
    /// Soft reset triggered through CTRL-AP
    CtrlAP,
    APPROTECTSTATUS,
    0x00C,
    [(APPROTECTSTATUS: bool),],
    value,
    APPROTECTSTATUS {
        APPROTECTSTATUS: if value == 1 { true } else { false },
    },
    if value.APPROTECTSTATUS { 1 } else { 0 }
);
