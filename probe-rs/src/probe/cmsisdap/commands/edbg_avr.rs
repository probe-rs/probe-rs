use super::{
    CmsisDapDevice, CmsisDapError, SendError, Status,
    general::{
        connect::{ConnectRequest, ConnectResponse},
        disconnect::{DisconnectRequest, DisconnectResponse},
        host_status::HostStatusRequest,
        info::{
            FirmwareVersionCommand, PacketSizeCommand, ProductIdCommand, SerialNumberCommand,
            VendorCommand,
        },
        reset::{ResetRequest, ResetResponse},
    },
    send_command,
};
use crate::probe::{DebugProbeError, DebugProbeSelector, ProbeError};

const PKOBN_UPDI_VID: u16 = 0x03eb;
const PKOBN_UPDI_PID: u16 = 0x2175;

const TOKEN: u8 = 0x0e;

const SCOPE_INFO: u8 = 0x00;
const SCOPE_GENERAL: u8 = 0x01;
const SCOPE_AVR: u8 = 0x12;

const CMD3_GET_INFO: u8 = 0x00;
const CMD3_SET_PARAMETER: u8 = 0x01;
const CMD3_GET_PARAMETER: u8 = 0x02;
const CMD3_SIGN_ON: u8 = 0x10;
const CMD3_SIGN_OFF: u8 = 0x11;
const CMD3_ENTER_PROGMODE: u8 = 0x15;
const CMD3_LEAVE_PROGMODE: u8 = 0x16;
const CMD3_ERASE_MEMORY: u8 = 0x20;
const CMD3_READ_MEMORY: u8 = 0x21;
const CMD3_WRITE_MEMORY: u8 = 0x23;

const CMD3_INFO_SERIAL: u8 = 0x81;

const RSP3_OK: u8 = 0x80;
const RSP3_INFO: u8 = 0x81;
const RSP3_DATA: u8 = 0x84;
const RSP3_STATUS_MASK: u8 = 0xe0;

const XMEGA_ERASE_CHIP: u8 = 0x00;
const XMEGA_ERASE_APP_PAGE: u8 = 0x04;

const MTYPE_EEPROM: u8 = 0x22;
const MTYPE_FLASH_PAGE: u8 = 0xb0;
const MTYPE_FUSE_BITS: u8 = 0xb2;
const MTYPE_LOCK_BITS: u8 = 0xb3;
const MTYPE_SRAM: u8 = 0x20;
const MTYPE_FLASH: u8 = 0xc0;
const MTYPE_USERSIG: u8 = 0xc5;
const MTYPE_PRODSIG: u8 = 0xc6;
const MTYPE_SIB: u8 = 0xd3;

const PARM3_HW_VER: u8 = 0x00;
const PARM3_VTARGET: u8 = 0x00;
const PARM3_DEVICEDESC: u8 = 0x00;
const PARM3_ARCH: u8 = 0x00;
const PARM3_ARCH_UPDI: u8 = 5;
const PARM3_SESS_PURPOSE: u8 = 0x01;
const PARM3_SESS_PROGRAMMING: u8 = 1;
const PARM3_CONNECTION: u8 = 0x00;
const PARM3_CONN_UPDI: u8 = 8;
const PARM3_CLK_XMEGA_PDI: u8 = 0x31;

const EDBG_VENDOR_AVR_CMD: u8 = 0x80;
const EDBG_VENDOR_AVR_RSP: u8 = 0x81;

const DEFAULT_MINIMUM_CHARACTERISED_DIV1_VOLTAGE_MV: u16 = 4500;
const DEFAULT_MINIMUM_CHARACTERISED_DIV2_VOLTAGE_MV: u16 = 2700;
const DEFAULT_MINIMUM_CHARACTERISED_DIV4_VOLTAGE_MV: u16 = 2200;
const DEFAULT_MINIMUM_CHARACTERISED_DIV8_VOLTAGE_MV: u16 = 1500;
const MAX_FREQUENCY_SHARED_UPDI_PIN: u16 = 750;
const FUSES_SYSCFG0_OFFSET: u8 = 5;

const AVR_SIBLEN: usize = 32;

/// Device-specific parameters for a UPDI-attached AVR chip.
#[derive(Debug, Clone)]
pub struct AvrChipDescriptor {
    /// Human-readable chip name (e.g. "ATmega4809").
    pub name: &'static str,
    /// Three-byte device signature read from production signature row.
    pub signature: [u8; 3],
    /// Flash memory base address in the data space.
    pub flash_base: u32,
    /// Flash memory size in bytes.
    pub flash_size: u32,
    /// Flash page size in bytes.
    pub flash_page_size: u32,
    /// EEPROM base address in the data space.
    pub eeprom_base: u32,
    /// EEPROM size in bytes.
    pub eeprom_size: u32,
    /// EEPROM page size in bytes (for page-write granularity).
    pub eeprom_page_size: u8,
    /// Fuse region base address.
    pub fuses_base: u32,
    /// Number of fuse bytes.
    pub fuses_size: u32,
    /// Lock bit region base address.
    pub lock_base: u32,
    /// Lock region size in bytes.
    pub lock_size: u32,
    /// User row (USERROW) base address.
    pub userrow_base: u32,
    /// User row size in bytes.
    pub userrow_size: u32,
    /// Signature / production signature row base address.
    pub signature_base: u32,
    /// Production signature row size in bytes.
    pub prodsig_size: u32,
    /// NVM controller base address.
    pub nvm_base: u16,
    /// OCD (on-chip debug) module base address.
    pub ocd_base: u16,
    /// SYSCFG peripheral base address.
    pub syscfg_base: u32,
    /// Address mode: 0 = 16-bit, 1 = 24-bit.
    pub address_mode: u8,
    /// HVUPDI variant identifier sent in the device descriptor.
    pub hvupdi_variant: u8,
}

/// Chip descriptor for the ATmega4809 (megaAVR 0-series, 48 KB flash).
pub const ATMEGA4809: AvrChipDescriptor = AvrChipDescriptor {
    name: "ATmega4809",
    signature: [0x1e, 0x96, 0x51],
    flash_base: 0x4000,
    flash_size: 0xC000,
    flash_page_size: 128,
    eeprom_base: 0x1400,
    eeprom_size: 256,
    eeprom_page_size: 64,
    fuses_base: 0x1280,
    fuses_size: 10,
    lock_base: 0x128A,
    lock_size: 1,
    userrow_base: 0x1300,
    userrow_size: 64,
    signature_base: 0x1100,
    prodsig_size: 128,
    nvm_base: 0x1000,
    ocd_base: 0x0F80,
    syscfg_base: 0x0F00,
    address_mode: 0,
    hvupdi_variant: 1,
};

/// Chip descriptor for the AVR128DB48 (AVR DB-series, 128 KB flash).
pub const AVR128DB48: AvrChipDescriptor = AvrChipDescriptor {
    name: "AVR128DB48",
    signature: [0x1e, 0x97, 0x0c],
    flash_base: 0x800000,
    flash_size: 0x20000,
    flash_page_size: 512,
    eeprom_base: 0x1400,
    eeprom_size: 512,
    eeprom_page_size: 1,
    fuses_base: 0x1050,
    fuses_size: 16,
    lock_base: 0x1040,
    lock_size: 4,
    userrow_base: 0x1080,
    userrow_size: 32,
    signature_base: 0x1100,
    prodsig_size: 128,
    nvm_base: 0x1000,
    ocd_base: 0x0F80,
    syscfg_base: 0x0F00,
    address_mode: 1,
    hvupdi_variant: 1,
};

/// Chip descriptor for the AVR64EA48 (AVR EA-series, 64 KB flash).
pub const AVR64EA48: AvrChipDescriptor = AvrChipDescriptor {
    name: "AVR64EA48",
    signature: [0x1e, 0x96, 0x1e],
    flash_base: 0x800000,
    flash_size: 0x10000,
    flash_page_size: 128,
    eeprom_base: 0x1400,
    eeprom_size: 512,
    eeprom_page_size: 8,
    fuses_base: 0x1050,
    fuses_size: 16,
    lock_base: 0x1040,
    lock_size: 4,
    userrow_base: 0x1080,
    userrow_size: 64,
    signature_base: 0x1100,
    prodsig_size: 128,
    nvm_base: 0x1000,
    ocd_base: 0x0F80,
    syscfg_base: 0x0F00,
    address_mode: 1,
    hvupdi_variant: 2,
};

/// All known AVR UPDI chip descriptors, tried in order during auto-detection.
pub const KNOWN_AVR_CHIPS: &[AvrChipDescriptor] = &[ATMEGA4809, AVR128DB48, AVR64EA48];

/// Look up a chip descriptor by its three-byte device signature.
pub fn lookup_avr_chip(signature: &[u8; 3]) -> Option<&'static AvrChipDescriptor> {
    KNOWN_AVR_CHIPS.iter().find(|c| &c.signature == signature)
}

/// Firmware version reported by the EDBG/JTAG3 general parameter block.
#[derive(Debug, Clone)]
pub struct IceFirmwareVersion {
    /// Hardware version number.
    pub hardware: u8,
    /// Major firmware version number.
    pub major: u8,
    /// Minor firmware version number.
    pub minor: u8,
    /// Firmware release/build number.
    pub release: u16,
}

/// Narrow information block returned by the experimental `pkobn_updi` query path.
#[derive(Debug, Clone)]
pub struct PkobnUpdiInfo {
    /// Probe selector used to open the probe.
    pub probe_selector: DebugProbeSelector,
    /// CMSIS-DAP vendor string.
    pub cmsis_dap_vendor: Option<String>,
    /// CMSIS-DAP product string.
    pub cmsis_dap_product: Option<String>,
    /// CMSIS-DAP serial number.
    pub cmsis_dap_serial: Option<String>,
    /// CMSIS-DAP firmware version string.
    pub cmsis_dap_firmware_version: Option<String>,
    /// CMSIS-DAP packet size in bytes.
    pub cmsis_dap_packet_size: u16,
    /// EDBG serial number returned by the AVR info scope.
    pub ice_serial: Option<String>,
    /// EDBG firmware version reported by the general parameter block.
    pub ice_firmware_version: IceFirmwareVersion,
    /// Target voltage in millivolts.
    pub target_voltage_mv: u16,
    /// UPDI clock in kilohertz.
    pub updi_clock_khz: u16,
    /// Partial family identifier returned by AVR sign-on.
    pub partial_family_id: Option<String>,
    /// UPDI SIB string.
    pub sib_string: String,
    /// Raw chip revision byte.
    pub chip_revision: u8,
    /// Raw three-byte device signature.
    pub signature: [u8; 3],
    /// Raw lock bytes (may be 1 or more depending on the chip).
    pub lock_bytes: Vec<u8>,
    /// Raw fuse bytes.
    pub fuses: Vec<u8>,
    /// Raw USERROW bytes.
    pub userrow: Vec<u8>,
    /// Raw production signature bytes.
    pub prodsig: Vec<u8>,
    /// Resolved chip descriptor when the signature matches a known target.
    pub chip: Option<&'static AvrChipDescriptor>,
}

/// Memory region within the narrow AVR UPDI implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvrMemoryRegion {
    /// Flash memory, addressed relative to the start of flash.
    Flash,
    /// EEPROM memory, addressed relative to the start of the EEPROM region.
    Eeprom,
    /// Fuse bytes, addressed relative to the fuse region start.
    Fuses,
    /// Lock byte, addressed relative to the lock region start.
    Lock,
    /// USERROW bytes, addressed relative to the user row start.
    UserRow,
    /// Production signature bytes, addressed relative to the production signature start.
    ProdSig,
}

#[derive(Debug, thiserror::Error, docsplay::Display)]
pub enum EdbgAvrError {
    /// Error while using the CMSIS-DAP transport layer.
    CmsisDap(#[from] CmsisDapError),
    /// Error while using the probe transport.
    Transport(#[from] SendError),
    /// The selected probe is not supported by the narrow EDBG AVR path: {selector}
    UnsupportedProbe { selector: DebugProbeSelector },
    /// Unexpected EDBG AVR response while handling {context}: {details}
    UnexpectedResponse {
        context: &'static str,
        details: String,
    },
    /// EDBG AVR command {context} failed with status code 0x{code:02x}
    CommandFailed { context: &'static str, code: u8 },
    /// Requested read extends past the end of the selected {region:?} region.
    OutOfRange {
        region: AvrMemoryRegion,
        offset: u32,
        length: u32,
        region_size: u32,
    },
}

impl ProbeError for EdbgAvrError {}

/// Query a Microchip `pkobn_updi` / `03eb:2175` probe with auto-detected AVR target.
pub fn query_pkobn_updi(selector: &DebugProbeSelector) -> Result<PkobnUpdiInfo, DebugProbeError> {
    if selector.vendor_id != PKOBN_UPDI_VID || selector.product_id != PKOBN_UPDI_PID {
        return Err(EdbgAvrError::UnsupportedProbe {
            selector: selector.clone(),
        }
        .into());
    }

    let mut device = super::super::tools::open_device_from_selector(selector)?;
    device.drain();
    let packet_size = device.find_packet_size()? as u16;

    let cmsis_dap_vendor = trim_probe_string(send_command(&mut device, &VendorCommand {})?);
    let cmsis_dap_product = trim_probe_string(send_command(&mut device, &ProductIdCommand {})?);
    let cmsis_dap_serial = trim_probe_string(send_command(&mut device, &SerialNumberCommand {})?);
    let cmsis_dap_firmware_version =
        trim_probe_string(send_command(&mut device, &FirmwareVersionCommand {})?);
    let _ = send_command(&mut device, &PacketSizeCommand {})?;

    let mut transport = EdbgAvrTransport::new(device);

    let result = transport.query_target(PkobnUpdiInfo {
        probe_selector: selector.clone(),
        cmsis_dap_vendor,
        cmsis_dap_product,
        cmsis_dap_serial,
        cmsis_dap_firmware_version,
        cmsis_dap_packet_size: packet_size,
        ice_serial: None,
        ice_firmware_version: IceFirmwareVersion {
            hardware: 0,
            major: 0,
            minor: 0,
            release: 0,
        },
        target_voltage_mv: 0,
        updi_clock_khz: 0,
        partial_family_id: None,
        sib_string: String::new(),
        chip_revision: 0,
        signature: [0; 3],
        lock_bytes: vec![],
        fuses: vec![],
        userrow: vec![],
        prodsig: vec![],
        chip: None,
    });

    finish_transport(&mut transport, result).map_err(DebugProbeError::from)
}

/// Read bytes from an AVR UPDI memory region.
pub fn read_pkobn_updi_region(
    selector: &DebugProbeSelector,
    region: AvrMemoryRegion,
    offset: u32,
    length: u32,
) -> Result<Vec<u8>, DebugProbeError> {
    let mut transport = open_pkobn_transport(selector)?;
    let result = (|| {
        let _ = transport.auto_detect_and_enter()?;
        transport.read_region(region, offset, length)
    })();

    finish_transport(&mut transport, result).map_err(DebugProbeError::from)
}

/// Read bytes from an AVR UPDI memory region using an already-open CMSIS-DAP probe.
///
/// The chip descriptor was already verified during session creation
/// (`auto_determine_target` -> `identify_attached_pkobn_updi` -> `auto_detect_and_enter`),
/// so we skip signature verification here and go straight to `enter_programming_session`.
pub fn read_attached_pkobn_updi_region(
    probe: &mut super::super::CmsisDap,
    chip: &'static AvrChipDescriptor,
    region: AvrMemoryRegion,
    offset: u32,
    length: u32,
) -> Result<Vec<u8>, DebugProbeError> {
    let mut transport = EdbgAvrTransport::from_attached_device(&mut probe.device, chip);
    let result = (|| {
        // Use enter_programming_session (not auto_detect_and_enter) since the chip
        // descriptor was already verified during session creation.
        let _ = transport.enter_programming_session()?;
        transport.read_region(region, offset, length)
    })();

    finish_transport(&mut transport, result).map_err(DebugProbeError::from)
}

/// Erase the AVR target through the EDBG AVR chip erase command using an
/// already-open CMSIS-DAP probe.
///
/// The chip descriptor was already verified during session creation
/// (`auto_determine_target` -> `identify_attached_pkobn_updi` -> `auto_detect_and_enter`).
pub fn erase_attached_pkobn_updi(
    probe: &mut super::super::CmsisDap,
    chip: &'static AvrChipDescriptor,
) -> Result<(), DebugProbeError> {
    let mut transport = EdbgAvrTransport::from_attached_device(&mut probe.device, chip);
    let result = (|| {
        let _ = transport.enter_programming_session()?;
        transport.erase_chip()
    })();

    finish_transport(&mut transport, result).map_err(DebugProbeError::from)
}

/// Write bytes into AVR flash memory using page programming through an
/// already-open CMSIS-DAP probe.
///
/// The chip descriptor was already verified during session creation
/// (`auto_determine_target` -> `identify_attached_pkobn_updi` -> `auto_detect_and_enter`).
pub fn write_attached_pkobn_updi_flash(
    probe: &mut super::super::CmsisDap,
    chip: &'static AvrChipDescriptor,
    offset: u32,
    data: &[u8],
) -> Result<(), DebugProbeError> {
    let mut transport = EdbgAvrTransport::from_attached_device(&mut probe.device, chip);
    let result = (|| {
        let _ = transport.enter_programming_session()?;
        transport.write_flash(offset, data)
    })();

    finish_transport(&mut transport, result).map_err(DebugProbeError::from)
}

/// Query the AVR target through an already-open CMSIS-DAP probe.
///
/// CMSIS-DAP info strings (vendor, product, serial, firmware version) are not
/// available through this path because they were already consumed during probe
/// initialisation; those fields will be `None` in the returned struct.
pub fn query_attached_pkobn_updi(
    probe: &mut super::super::CmsisDap,
) -> Result<PkobnUpdiInfo, DebugProbeError> {
    let mut transport = EdbgAvrTransport::from_attached_device(&mut probe.device, &ATMEGA4809);
    let selector = DebugProbeSelector {
        vendor_id: PKOBN_UPDI_VID,
        product_id: PKOBN_UPDI_PID,
        interface: None,
        serial_number: None,
    };
    let result = transport.query_target(PkobnUpdiInfo {
        probe_selector: selector,
        cmsis_dap_vendor: None,
        cmsis_dap_product: None,
        cmsis_dap_serial: None,
        cmsis_dap_firmware_version: None,
        cmsis_dap_packet_size: probe.packet_size,
        ice_serial: None,
        ice_firmware_version: IceFirmwareVersion {
            hardware: 0,
            major: 0,
            minor: 0,
            release: 0,
        },
        target_voltage_mv: 0,
        updi_clock_khz: 0,
        partial_family_id: None,
        sib_string: String::new(),
        chip_revision: 0,
        signature: [0; 3],
        lock_bytes: vec![],
        fuses: vec![],
        userrow: vec![],
        prodsig: vec![],
        chip: None,
    });

    finish_transport(&mut transport, result).map_err(DebugProbeError::from)
}

/// Write bytes into AVR flash memory using page programming.
pub fn write_pkobn_updi_flash(
    selector: &DebugProbeSelector,
    offset: u32,
    data: &[u8],
) -> Result<(), DebugProbeError> {
    let mut transport = open_pkobn_transport(selector)?;
    let result = (|| {
        let _ = transport.auto_detect_and_enter()?;
        transport.write_flash(offset, data)
    })();

    finish_transport(&mut transport, result).map_err(DebugProbeError::from)
}

/// Erase the AVR target through the EDBG AVR chip erase command.
pub fn erase_pkobn_updi(selector: &DebugProbeSelector) -> Result<(), DebugProbeError> {
    let mut transport = open_pkobn_transport(selector)?;
    let result = (|| {
        let _ = transport.auto_detect_and_enter()?;
        transport.erase_chip()
    })();

    finish_transport(&mut transport, result).map_err(DebugProbeError::from)
}

/// Reset a Microchip `pkobn_updi` / `03eb:2175` probe through the generic CMSIS-DAP reset command.
pub fn reset_pkobn_updi(selector: &DebugProbeSelector) -> Result<(), DebugProbeError> {
    if selector.vendor_id != PKOBN_UPDI_VID || selector.product_id != PKOBN_UPDI_PID {
        return Err(EdbgAvrError::UnsupportedProbe {
            selector: selector.clone(),
        }
        .into());
    }

    let mut device = super::super::tools::open_device_from_selector(selector)?;
    device.drain();
    let _ = device.find_packet_size()?;
    let _ = send_command(&mut device, &ResetRequest {})? as ResetResponse;
    Ok(())
}

/// Identify the AVR chip attached to a CMSIS-DAP probe by trying each known descriptor.
pub fn identify_attached_pkobn_updi(
    probe: &mut super::super::CmsisDap,
) -> Result<&'static AvrChipDescriptor, DebugProbeError> {
    let mut transport = EdbgAvrTransport::from_attached_device(&mut probe.device, &ATMEGA4809);
    let result = (|| {
        let _ = transport.auto_detect_and_enter()?;
        // Verify the chip by reading the production signature, matching what
        // query_target() does. This guards against multiple descriptors
        // succeeding sign-on but only one matching the actual device.
        let prodsig = transport.read_memory(MTYPE_PRODSIG, transport.chip.signature_base, 3)?;
        if prodsig.len() < 3 {
            return Err(EdbgAvrError::UnexpectedResponse {
                context: "chip identification",
                details: format!("expected at least 3 bytes, got {}", prodsig.len()),
            });
        }
        let signature: [u8; 3] = [prodsig[0], prodsig[1], prodsig[2]];
        lookup_avr_chip(&signature).ok_or_else(|| EdbgAvrError::UnexpectedResponse {
            context: "chip identification",
            details: format!(
                "unknown AVR signature: {:02x} {:02x} {:02x}",
                signature[0], signature[1], signature[2]
            ),
        })
    })();
    finish_transport(&mut transport, result).map_err(DebugProbeError::from)
}

fn open_pkobn_transport(
    selector: &DebugProbeSelector,
) -> Result<EdbgAvrTransport<'static>, DebugProbeError> {
    if selector.vendor_id != PKOBN_UPDI_VID || selector.product_id != PKOBN_UPDI_PID {
        return Err(EdbgAvrError::UnsupportedProbe {
            selector: selector.clone(),
        }
        .into());
    }

    let mut device = super::super::tools::open_device_from_selector(selector)?;
    device.drain();
    let _ = device.find_packet_size()?;
    Ok(EdbgAvrTransport::new(device))
}

enum TransportDevice<'a> {
    Owned(CmsisDapDevice),
    Borrowed(&'a mut CmsisDapDevice),
}

impl TransportDevice<'_> {
    fn as_mut(&mut self) -> &mut CmsisDapDevice {
        match self {
            TransportDevice::Owned(device) => device,
            TransportDevice::Borrowed(device) => device,
        }
    }

    fn as_ref(&self) -> &CmsisDapDevice {
        match self {
            TransportDevice::Owned(device) => device,
            TransportDevice::Borrowed(device) => device,
        }
    }
}

struct EdbgAvrTransport<'a> {
    device: TransportDevice<'a>,
    command_sequence: u16,
    prepared: bool,
    cleanup_prepare: bool,
    general_signed_on: bool,
    avr_signed_on: bool,
    programming_enabled: bool,
    chip: &'static AvrChipDescriptor,
}

impl EdbgAvrTransport<'static> {
    fn new(device: CmsisDapDevice) -> Self {
        Self {
            device: TransportDevice::Owned(device),
            command_sequence: 0,
            prepared: false,
            cleanup_prepare: false,
            general_signed_on: false,
            avr_signed_on: false,
            programming_enabled: false,
            chip: &ATMEGA4809,
        }
    }
}

impl<'a> EdbgAvrTransport<'a> {
    fn from_attached_device(
        device: &'a mut CmsisDapDevice,
        chip: &'static AvrChipDescriptor,
    ) -> Self {
        Self {
            device: TransportDevice::Borrowed(device),
            command_sequence: 0,
            prepared: true,
            cleanup_prepare: false,
            general_signed_on: false,
            avr_signed_on: false,
            programming_enabled: false,
            chip,
        }
    }

    fn query_target(&mut self, mut info: PkobnUpdiInfo) -> Result<PkobnUpdiInfo, EdbgAvrError> {
        let partial_family_id = self.auto_detect_and_enter()?;
        info.ice_firmware_version = self.read_ice_firmware_version()?;
        info.ice_serial = self.get_info_string(CMD3_INFO_SERIAL)?;
        info.target_voltage_mv = self.get_u16_param(SCOPE_GENERAL, 1, PARM3_VTARGET)?;
        info.updi_clock_khz = self.get_u16_param(SCOPE_AVR, 1, PARM3_CLK_XMEGA_PDI)?;
        info.partial_family_id = partial_family_id;

        info.sib_string = trim_ascii_nul(self.read_memory(MTYPE_SIB, 0, AVR_SIBLEN as u32)?);

        // Note: enter_progmode() is already called by auto_detect_and_enter() above.

        let chip_revision = self.read_memory(MTYPE_SRAM, self.chip.syscfg_base + 1, 1)?;
        if chip_revision.len() != 1 {
            return Err(EdbgAvrError::UnexpectedResponse {
                context: "chip revision",
                details: format!("expected 1 byte, got {}", chip_revision.len()),
            });
        }
        info.chip_revision = chip_revision[0];

        let lock = self.read_region(AvrMemoryRegion::Lock, 0, self.chip.lock_size)?;
        if lock.len() != self.chip.lock_size as usize {
            return Err(EdbgAvrError::UnexpectedResponse {
                context: "lock read",
                details: format!(
                    "expected {} byte(s), got {}",
                    self.chip.lock_size,
                    lock.len()
                ),
            });
        }
        info.lock_bytes = lock;

        info.fuses = self.read_region(AvrMemoryRegion::Fuses, 0, self.chip.fuses_size)?;
        if info.fuses.len() != self.chip.fuses_size as usize {
            return Err(EdbgAvrError::UnexpectedResponse {
                context: "fuse read",
                details: format!(
                    "expected {} bytes, got {}",
                    self.chip.fuses_size,
                    info.fuses.len()
                ),
            });
        }

        info.userrow = self.read_region(AvrMemoryRegion::UserRow, 0, self.chip.userrow_size)?;
        if info.userrow.len() != self.chip.userrow_size as usize {
            return Err(EdbgAvrError::UnexpectedResponse {
                context: "user row read",
                details: format!(
                    "expected {} bytes, got {}",
                    self.chip.userrow_size,
                    info.userrow.len()
                ),
            });
        }

        info.prodsig = self.read_region(AvrMemoryRegion::ProdSig, 0, self.chip.prodsig_size)?;
        if info.prodsig.len() < 3 {
            return Err(EdbgAvrError::UnexpectedResponse {
                context: "production signature read",
                details: format!("expected at least 3 bytes, got {}", info.prodsig.len()),
            });
        }
        info.signature.copy_from_slice(&info.prodsig[..3]);
        info.chip = lookup_avr_chip(&info.signature);

        Ok(info)
    }

    /// Try each known chip descriptor in turn, returning the partial family ID on success.
    fn auto_detect_and_enter(&mut self) -> Result<Option<String>, EdbgAvrError> {
        if self.programming_enabled {
            return Ok(None);
        }

        tracing::debug!("EDBG AVR: auto-detecting chip and entering programming session");
        self.prepare()?;
        self.general_sign_on()?;

        self.set_param(SCOPE_AVR, 0, PARM3_ARCH, &[PARM3_ARCH_UPDI])?;
        self.set_param(SCOPE_AVR, 0, PARM3_SESS_PURPOSE, &[PARM3_SESS_PROGRAMMING])?;
        self.set_param(SCOPE_AVR, 1, PARM3_CONNECTION, &[PARM3_CONN_UPDI])?;

        for chip in KNOWN_AVR_CHIPS {
            tracing::debug!("EDBG AVR: trying chip descriptor for {}", chip.name);
            self.set_param(
                SCOPE_AVR,
                2,
                PARM3_DEVICEDESC,
                &build_updi_device_descriptor(chip),
            )?;

            match self.command(&[SCOPE_AVR, CMD3_SIGN_ON, 0, 0], "AVR sign-on") {
                Ok(avr_sign_on_response) => {
                    tracing::info!("EDBG AVR: sign-on succeeded with {}", chip.name);
                    self.avr_signed_on = true;
                    self.chip = chip;
                    let partial_family_id = partial_family_id_from_response(&avr_sign_on_response);
                    self.enter_progmode()?;

                    // Verify chip identity by reading the production signature.
                    let prodsig = self.read_memory(MTYPE_PRODSIG, self.chip.signature_base, 3)?;
                    if prodsig.len() < 3 {
                        return Err(EdbgAvrError::UnexpectedResponse {
                            context: "signature verification",
                            details: format!("expected at least 3 bytes, got {}", prodsig.len()),
                        });
                    }
                    let signature: [u8; 3] = [prodsig[0], prodsig[1], prodsig[2]];
                    if signature != self.chip.signature {
                        tracing::warn!(
                            "EDBG AVR: sign-on accepted {} but signature {:02x} {:02x} {:02x} \
                             does not match expected {:02x} {:02x} {:02x}",
                            self.chip.name,
                            signature[0],
                            signature[1],
                            signature[2],
                            self.chip.signature[0],
                            self.chip.signature[1],
                            self.chip.signature[2],
                        );
                        let actual_chip = lookup_avr_chip(&signature).ok_or_else(|| {
                            EdbgAvrError::UnexpectedResponse {
                                context: "signature verification",
                                details: format!(
                                    "unknown AVR signature: {:02x} {:02x} {:02x}",
                                    signature[0], signature[1], signature[2]
                                ),
                            }
                        })?;

                        // Re-send the correct descriptor so firmware and driver
                        // agree on flash geometry, then re-enter programming mode.
                        tracing::info!(
                            "EDBG AVR: re-initializing with correct descriptor for {}",
                            actual_chip.name
                        );
                        self.leave_progmode()?;
                        self.avr_sign_off()?;
                        self.chip = actual_chip;
                        self.set_param(
                            SCOPE_AVR,
                            2,
                            PARM3_DEVICEDESC,
                            &build_updi_device_descriptor(self.chip),
                        )?;
                        let re_sign_on =
                            self.command(&[SCOPE_AVR, CMD3_SIGN_ON, 0, 0], "AVR re-sign-on")?;
                        self.avr_signed_on = true;
                        let partial_family_id = partial_family_id_from_response(&re_sign_on);
                        self.enter_progmode()?;
                        return Ok(partial_family_id);
                    }

                    return Ok(partial_family_id);
                }
                Err(e) => {
                    tracing::debug!(
                        "EDBG AVR: sign-on with {} failed: {e}, trying next",
                        chip.name
                    );
                    // Sign-off so we can try a different descriptor
                    let _ = self.command(&[SCOPE_AVR, CMD3_SIGN_OFF, 0], "AVR sign-off (retry)");
                }
            }
        }

        Err(EdbgAvrError::UnexpectedResponse {
            context: "auto-detect",
            details: "no known chip descriptor was accepted by the target".to_string(),
        })
    }

    fn enter_programming_session(&mut self) -> Result<Option<String>, EdbgAvrError> {
        if self.programming_enabled {
            return Ok(None);
        }

        tracing::debug!(
            "EDBG AVR: entering programming session for {}",
            self.chip.name
        );
        self.prepare()?;
        self.general_sign_on()?;

        self.set_param(SCOPE_AVR, 0, PARM3_ARCH, &[PARM3_ARCH_UPDI])?;
        self.set_param(SCOPE_AVR, 0, PARM3_SESS_PURPOSE, &[PARM3_SESS_PROGRAMMING])?;
        self.set_param(SCOPE_AVR, 1, PARM3_CONNECTION, &[PARM3_CONN_UPDI])?;
        self.set_param(
            SCOPE_AVR,
            2,
            PARM3_DEVICEDESC,
            &build_updi_device_descriptor(self.chip),
        )?;

        let avr_sign_on_response = self.command(&[SCOPE_AVR, CMD3_SIGN_ON, 0, 0], "AVR sign-on")?;
        self.avr_signed_on = true;
        let partial_family_id = partial_family_id_from_response(&avr_sign_on_response);
        self.enter_progmode()?;

        Ok(partial_family_id)
    }

    fn read_region(
        &mut self,
        region: AvrMemoryRegion,
        offset: u32,
        length: u32,
    ) -> Result<Vec<u8>, EdbgAvrError> {
        let (memory_type, base, region_size) = match region {
            AvrMemoryRegion::Flash => (MTYPE_FLASH_PAGE, 0, self.chip.flash_size),
            AvrMemoryRegion::Eeprom => (MTYPE_EEPROM, self.chip.eeprom_base, self.chip.eeprom_size),
            AvrMemoryRegion::Fuses => (MTYPE_FUSE_BITS, self.chip.fuses_base, self.chip.fuses_size),
            AvrMemoryRegion::Lock => (MTYPE_LOCK_BITS, self.chip.lock_base, self.chip.lock_size),
            AvrMemoryRegion::UserRow => (
                MTYPE_USERSIG,
                self.chip.userrow_base,
                self.chip.userrow_size,
            ),
            AvrMemoryRegion::ProdSig => (
                MTYPE_PRODSIG,
                self.chip.signature_base,
                self.chip.prodsig_size,
            ),
        };

        if offset > region_size || length > region_size.saturating_sub(offset) {
            return Err(EdbgAvrError::OutOfRange {
                region,
                offset,
                length,
                region_size,
            });
        }

        // The EDBG protocol encodes fragment counts in nibbles (max 15).
        // Large reads must be chunked to stay within the response fragment limit.
        let max_chunk = self.max_read_length();
        if length <= max_chunk {
            return self.read_memory(memory_type, base + offset, length);
        }

        let mut result = Vec::with_capacity(length as usize);
        let mut remaining = length;
        let mut addr = base + offset;
        while remaining > 0 {
            let chunk = remaining.min(max_chunk);
            result.extend_from_slice(&self.read_memory(memory_type, addr, chunk)?);
            addr += chunk;
            remaining -= chunk;
        }
        Ok(result)
    }

    /// Maximum number of bytes that can be read in a single `read_memory` call
    /// without exceeding the EDBG 15-fragment response limit.
    fn max_read_length(&self) -> u32 {
        let packet_size = self.packet_size();
        // Each response fragment carries (packet_size - 4) bytes of reassembled payload.
        // The first 3 bytes of the reassembled payload are TOKEN + sequence(2).
        // read_memory response has 3 header bytes (scope, rsp_type, 0) before data.
        // So usable data = 15 * (packet_size - 4) - 3 (token+seq) - 3 (response header).
        let per_fragment = packet_size.saturating_sub(4);
        let total_payload = 15 * per_fragment;
        let overhead = 6; // 3 for token+seq, 3 for read_memory response header
        let max = total_payload.saturating_sub(overhead);
        // Use a conservative round-down to avoid edge cases; floor to 256-byte boundary.
        let max = (max / 256) * 256;
        if max == 0 {
            per_fragment as u32
        } else {
            max as u32
        }
    }

    fn write_flash(&mut self, offset: u32, data: &[u8]) -> Result<(), EdbgAvrError> {
        tracing::debug!(
            "EDBG AVR: write_flash offset=0x{offset:04x} len={}",
            data.len()
        );
        let data_len = u32::try_from(data.len()).map_err(|_| EdbgAvrError::UnexpectedResponse {
            context: "write flash",
            details: format!("write length exceeds 32-bit range: {}", data.len()),
        })?;

        let flash_size = self.chip.flash_size;
        let flash_page_size = self.chip.flash_page_size;

        if offset > flash_size || data_len > flash_size.saturating_sub(offset) {
            return Err(EdbgAvrError::OutOfRange {
                region: AvrMemoryRegion::Flash,
                offset,
                length: data_len,
                region_size: flash_size,
            });
        }

        if data.is_empty() {
            return Ok(());
        }

        let first_page = offset / flash_page_size;
        let last_page = (offset + data_len - 1) / flash_page_size;

        for page in first_page..=last_page {
            let page_offset = page * flash_page_size;
            let page_end = page_offset + flash_page_size;

            let write_start = offset.max(page_offset);
            let write_end = (offset + data_len).min(page_end);

            let source_start = usize::try_from(write_start - offset).map_err(|_| {
                EdbgAvrError::UnexpectedResponse {
                    context: "write flash",
                    details: "source start offset conversion failed".to_string(),
                }
            })?;
            let source_end = usize::try_from(write_end - offset).map_err(|_| {
                EdbgAvrError::UnexpectedResponse {
                    context: "write flash",
                    details: "source end offset conversion failed".to_string(),
                }
            })?;
            let page_write_start = usize::try_from(write_start - page_offset).map_err(|_| {
                EdbgAvrError::UnexpectedResponse {
                    context: "write flash",
                    details: "page-relative start conversion failed".to_string(),
                }
            })?;
            let page_write_end = usize::try_from(write_end - page_offset).map_err(|_| {
                EdbgAvrError::UnexpectedResponse {
                    context: "write flash",
                    details: "page-relative end conversion failed".to_string(),
                }
            })?;

            let mut page_data =
                self.read_region(AvrMemoryRegion::Flash, page_offset, flash_page_size)?;
            if page_data.len() != flash_page_size as usize {
                return Err(EdbgAvrError::UnexpectedResponse {
                    context: "write flash",
                    details: format!(
                        "expected {} bytes of page data, got {}",
                        flash_page_size,
                        page_data.len()
                    ),
                });
            }

            let original_page_data = page_data.clone();

            page_data[page_write_start..page_write_end]
                .copy_from_slice(&data[source_start..source_end]);
            if page_data == original_page_data {
                continue;
            }

            self.erase_flash_page(page_offset)?;
            self.write_flash_page(page_offset, &page_data)?;
        }

        Ok(())
    }

    fn erase_chip(&mut self) -> Result<(), EdbgAvrError> {
        tracing::debug!("EDBG AVR: erasing chip");
        let mut command = Vec::with_capacity(8);
        command.extend_from_slice(&[SCOPE_AVR, CMD3_ERASE_MEMORY, 0, XMEGA_ERASE_CHIP]);
        command.extend_from_slice(&0u32.to_le_bytes());

        let _ = self.command(&command, "erase chip")?;
        Ok(())
    }

    fn erase_flash_page(&mut self, address: u32) -> Result<(), EdbgAvrError> {
        let mut command = Vec::with_capacity(8);
        command.extend_from_slice(&[SCOPE_AVR, CMD3_ERASE_MEMORY, 0, XMEGA_ERASE_APP_PAGE]);
        command.extend_from_slice(&address.to_le_bytes());

        let _ = self.command(&command, "erase flash page")?;
        Ok(())
    }

    fn write_flash_page(&mut self, address: u32, page_data: &[u8]) -> Result<(), EdbgAvrError> {
        let flash_page_size = self.chip.flash_page_size;
        if page_data.len() != flash_page_size as usize {
            return Err(EdbgAvrError::UnexpectedResponse {
                context: "write flash page",
                details: format!(
                    "expected page size {}, got {}",
                    flash_page_size,
                    page_data.len()
                ),
            });
        }

        let mut command = Vec::with_capacity(13 + page_data.len());
        command.extend_from_slice(&[SCOPE_AVR, CMD3_WRITE_MEMORY, 0, MTYPE_FLASH]);
        command.extend_from_slice(&address.to_le_bytes());
        command.extend_from_slice(&flash_page_size.to_le_bytes());
        command.push(0);
        command.extend_from_slice(page_data);

        let _ = self.command(&command, "write flash page")?;
        Ok(())
    }

    fn cleanup(&mut self) -> Result<(), EdbgAvrError> {
        let mut first_error = None;

        if self.programming_enabled
            && let Err(error) = self.leave_progmode()
        {
            first_error.get_or_insert(error);
        }
        if self.avr_signed_on
            && let Err(error) = self.avr_sign_off()
        {
            first_error.get_or_insert(error);
        }
        if self.general_signed_on
            && let Err(error) = self.general_sign_off()
        {
            first_error.get_or_insert(error);
        }
        if self.prepared
            && self.cleanup_prepare
            && let Err(error) = self.finish_prepare()
        {
            first_error.get_or_insert(error);
        }

        if let Some(error) = first_error {
            Err(error)
        } else {
            Ok(())
        }
    }

    fn prepare(&mut self) -> Result<(), EdbgAvrError> {
        if self.prepared {
            tracing::debug!("EDBG AVR: already prepared, skipping CMSIS-DAP connect");
            return Ok(());
        }
        tracing::debug!("EDBG AVR: preparing CMSIS-DAP connection");
        match send_command(self.device.as_mut(), &ConnectRequest::Swd)? {
            ConnectResponse::SuccessfulInitForSWD => {}
            ConnectResponse::SuccessfulInitForJTAG | ConnectResponse::InitFailed => {
                return Err(EdbgAvrError::UnexpectedResponse {
                    context: "CMSIS-DAP connect",
                    details: "probe did not enter SWD mode".to_string(),
                });
            }
        }

        // Mark prepared before host-status so cleanup() will disconnect
        // even if the host-status call fails.
        self.prepared = true;
        self.cleanup_prepare = true;
        let _ = send_command(self.device.as_mut(), &HostStatusRequest::connected(true))?;
        Ok(())
    }

    fn finish_prepare(&mut self) -> Result<(), EdbgAvrError> {
        // Best-effort: always attempt DisconnectRequest even if host-status fails,
        // otherwise the CMSIS-DAP link stays open for the next caller.
        let mut first_error =
            send_command(self.device.as_mut(), &HostStatusRequest::connected(false))
                .map(|_| ())
                .err()
                .map(EdbgAvrError::from);

        match send_command(self.device.as_mut(), &DisconnectRequest {}) {
            Ok(DisconnectResponse(Status::DapOk)) => {}
            Ok(DisconnectResponse(status)) => {
                first_error.get_or_insert(EdbgAvrError::UnexpectedResponse {
                    context: "CMSIS-DAP disconnect",
                    details: format!("unexpected disconnect status {status:?}"),
                });
            }
            Err(e) => {
                first_error.get_or_insert(e.into());
            }
        }

        self.prepared = false;
        match first_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn general_sign_on(&mut self) -> Result<(), EdbgAvrError> {
        let _ = self.command(&[SCOPE_GENERAL, CMD3_SIGN_ON, 0], "general sign-on")?;
        self.general_signed_on = true;
        Ok(())
    }

    fn general_sign_off(&mut self) -> Result<(), EdbgAvrError> {
        let _ = self.command(&[SCOPE_GENERAL, CMD3_SIGN_OFF, 0, 0], "general sign-off")?;
        self.general_signed_on = false;
        Ok(())
    }

    fn avr_sign_off(&mut self) -> Result<(), EdbgAvrError> {
        let _ = self.command(&[SCOPE_AVR, CMD3_SIGN_OFF, 0], "AVR sign-off")?;
        self.avr_signed_on = false;
        Ok(())
    }

    fn enter_progmode(&mut self) -> Result<(), EdbgAvrError> {
        let _ = self.command(&[SCOPE_AVR, CMD3_ENTER_PROGMODE, 0], "enter progmode")?;
        self.programming_enabled = true;
        Ok(())
    }

    fn leave_progmode(&mut self) -> Result<(), EdbgAvrError> {
        let _ = self.command(&[SCOPE_AVR, CMD3_LEAVE_PROGMODE, 0], "leave progmode")?;
        self.programming_enabled = false;
        Ok(())
    }

    fn read_ice_firmware_version(&mut self) -> Result<IceFirmwareVersion, EdbgAvrError> {
        let params = self.get_param(SCOPE_GENERAL, 0, PARM3_HW_VER, 5)?;
        if params.len() < 5 {
            return Err(EdbgAvrError::UnexpectedResponse {
                context: "ICE firmware version",
                details: format!("expected 5 bytes, got {}", params.len()),
            });
        }

        Ok(IceFirmwareVersion {
            hardware: params[0],
            major: params[1],
            minor: params[2],
            release: u16::from_le_bytes([params[3], params[4]]),
        })
    }

    fn get_info_string(&mut self, info_kind: u8) -> Result<Option<String>, EdbgAvrError> {
        let response = self.command(
            &[SCOPE_INFO, CMD3_GET_INFO, 0, info_kind],
            "get info string",
        )?;

        if response.len() < 3 || response[1] != RSP3_INFO {
            return Err(EdbgAvrError::UnexpectedResponse {
                context: "get info string",
                details: format!("unexpected response {:02x?}", response),
            });
        }

        Ok(trim_probe_string(Some(
            String::from_utf8_lossy(&response[3..]).into_owned(),
        )))
    }

    fn get_param(
        &mut self,
        scope: u8,
        section: u8,
        parameter: u8,
        length: u8,
    ) -> Result<Vec<u8>, EdbgAvrError> {
        let response = self.command(
            &[scope, CMD3_GET_PARAMETER, 0, section, parameter, length],
            "get parameter",
        )?;

        if response.len() < 3 || response[1] != RSP3_DATA {
            return Err(EdbgAvrError::UnexpectedResponse {
                context: "get parameter",
                details: format!("unexpected response {:02x?}", response),
            });
        }

        let mut value = response[3..].to_vec();
        value.truncate(length as usize);
        Ok(value)
    }

    fn get_u16_param(
        &mut self,
        scope: u8,
        section: u8,
        parameter: u8,
    ) -> Result<u16, EdbgAvrError> {
        let value = self.get_param(scope, section, parameter, 2)?;
        if value.len() < 2 {
            return Err(EdbgAvrError::UnexpectedResponse {
                context: "get 16-bit parameter",
                details: format!("expected 2 bytes, got {}", value.len()),
            });
        }

        Ok(u16::from_le_bytes([value[0], value[1]]))
    }

    fn set_param(
        &mut self,
        scope: u8,
        section: u8,
        parameter: u8,
        value: &[u8],
    ) -> Result<(), EdbgAvrError> {
        let length = u8::try_from(value.len()).map_err(|_| EdbgAvrError::UnexpectedResponse {
            context: "set parameter",
            details: format!("value too large: {}", value.len()),
        })?;

        let mut command = Vec::with_capacity(6 + value.len());
        command.extend_from_slice(&[scope, CMD3_SET_PARAMETER, 0, section, parameter, length]);
        command.extend_from_slice(value);
        let _ = self.command(&command, "set parameter")?;
        Ok(())
    }

    fn read_memory(
        &mut self,
        memory_type: u8,
        address: u32,
        length: u32,
    ) -> Result<Vec<u8>, EdbgAvrError> {
        tracing::trace!(
            "EDBG AVR: read_memory type=0x{memory_type:02x} addr=0x{address:04x} len={length}"
        );
        let mut command = Vec::with_capacity(12);
        command.extend_from_slice(&[SCOPE_AVR, CMD3_READ_MEMORY, 0, memory_type]);
        command.extend_from_slice(&address.to_le_bytes());
        command.extend_from_slice(&length.to_le_bytes());

        let response = self.command(&command, "read memory")?;
        if response.len() < 3 || response[1] != RSP3_DATA {
            return Err(EdbgAvrError::UnexpectedResponse {
                context: "read memory",
                details: format!("unexpected response {:02x?}", response),
            });
        }

        let payload = &response[3..];
        let expected_len = length as usize;
        if payload.len() < expected_len {
            return Err(EdbgAvrError::UnexpectedResponse {
                context: "read memory",
                details: format!(
                    "short read: expected {expected_len} bytes, got {}",
                    payload.len()
                ),
            });
        }
        Ok(payload[..expected_len].to_vec())
    }

    fn command(&mut self, payload: &[u8], context: &'static str) -> Result<Vec<u8>, EdbgAvrError> {
        self.send_payload(payload)?;
        let response = self.receive_payload()?;
        if response.len() < 2 {
            return Err(EdbgAvrError::UnexpectedResponse {
                context,
                details: format!("response too short: {:02x?}", response),
            });
        }

        if response[1] & RSP3_STATUS_MASK != RSP3_OK {
            let code = response.get(3).copied().unwrap_or(0);
            return Err(EdbgAvrError::CommandFailed { context, code });
        }

        Ok(response)
    }

    fn send_payload(&mut self, payload: &[u8]) -> Result<(), EdbgAvrError> {
        tracing::trace!("EDBG AVR: send_payload len={}", payload.len());
        let packet_size = self.packet_size();
        let first_capacity =
            packet_size
                .checked_sub(8)
                .ok_or_else(|| EdbgAvrError::UnexpectedResponse {
                    context: "EDBG send",
                    details: format!("packet size {packet_size} too small"),
                })?;
        let continuation_capacity =
            packet_size
                .checked_sub(4)
                .ok_or_else(|| EdbgAvrError::UnexpectedResponse {
                    context: "EDBG send",
                    details: format!("packet size {packet_size} too small"),
                })?;

        let nfragments = if payload.len() <= first_capacity {
            1usize
        } else {
            1 + (payload.len() - first_capacity).div_ceil(continuation_capacity)
        };
        if nfragments > 15 {
            return Err(EdbgAvrError::UnexpectedResponse {
                context: "EDBG send",
                details: format!(
                    "payload fragmented into {nfragments} packets, exceeds 4-bit fragment count limit of 15"
                ),
            });
        }
        let nfragments_u8 = nfragments as u8;

        let mut cursor = 0usize;

        for fragment_index in 0..nfragments {
            let fragment_number =
                u8::try_from(fragment_index + 1).map_err(|_| EdbgAvrError::UnexpectedResponse {
                    context: "EDBG send",
                    details: format!("invalid fragment number: {}", fragment_index + 1),
                })?;
            let is_first = fragment_index == 0;
            let is_last = fragment_index + 1 == nfragments;
            let max_chunk = if is_first {
                first_capacity
            } else {
                continuation_capacity
            };
            let chunk_len = (payload.len() - cursor).min(max_chunk);

            let mut packet = Vec::with_capacity(packet_size);
            packet.push(EDBG_VENDOR_AVR_CMD);
            packet.push((fragment_number << 4) | nfragments_u8);

            let fragment_len = if is_first { chunk_len + 4 } else { chunk_len };
            let fragment_len_u16 =
                u16::try_from(fragment_len).map_err(|_| EdbgAvrError::UnexpectedResponse {
                    context: "EDBG send",
                    details: format!("fragment too large: {fragment_len}"),
                })?;
            packet.extend_from_slice(&fragment_len_u16.to_be_bytes());

            if is_first {
                packet.push(TOKEN);
                packet.push(0);
                packet.extend_from_slice(&self.command_sequence.to_le_bytes());
            }

            packet.extend_from_slice(&payload[cursor..cursor + chunk_len]);
            cursor += chunk_len;

            let ack = self.transfer(&packet)?;
            if ack.first().copied() != Some(EDBG_VENDOR_AVR_CMD) {
                return Err(EdbgAvrError::UnexpectedResponse {
                    context: "EDBG send ack",
                    details: format!("unexpected ack {:02x?}", ack),
                });
            }
            if is_last && ack.get(1).copied() != Some(0x01) {
                return Err(EdbgAvrError::UnexpectedResponse {
                    context: "EDBG send ack",
                    details: format!("last-fragment completion missing in ack {:02x?}", ack),
                });
            }
        }

        Ok(())
    }

    fn receive_payload(&mut self) -> Result<Vec<u8>, EdbgAvrError> {
        const MAX_SEQUENCE_RETRIES: usize = 16;
        let mut retries = 0;
        loop {
            let mut collected = Vec::new();
            let mut fragment_count = None;
            let mut expected_fragment = 1u8;

            loop {
                let response = self.transfer(&[EDBG_VENDOR_AVR_RSP])?;
                if response.first().copied() != Some(EDBG_VENDOR_AVR_RSP) {
                    return Err(EdbgAvrError::UnexpectedResponse {
                        context: "EDBG receive",
                        details: format!("unexpected response prefix {:02x?}", response),
                    });
                }
                if response.len() < 2 {
                    return Err(EdbgAvrError::UnexpectedResponse {
                        context: "EDBG receive",
                        details: format!(
                            "vendor response too short: got {} byte(s), need at least 2",
                            response.len()
                        ),
                    });
                }

                if response[1] == 0 {
                    return Err(EdbgAvrError::UnexpectedResponse {
                        context: "EDBG receive",
                        details: "no response data available".to_string(),
                    });
                }

                let fragment_info = response[1];
                let total_fragments = fragment_info & 0x0f;
                let fragment_number = (fragment_info >> 4) & 0x0f;

                if let Some(existing_count) = fragment_count {
                    if existing_count != total_fragments {
                        return Err(EdbgAvrError::UnexpectedResponse {
                            context: "EDBG receive",
                            details: format!("inconsistent fragment count {:02x?}", response),
                        });
                    }
                } else {
                    fragment_count = Some(total_fragments);
                }

                if fragment_number != expected_fragment {
                    return Err(EdbgAvrError::UnexpectedResponse {
                        context: "EDBG receive",
                        details: format!(
                            "expected fragment {expected_fragment}, received {fragment_number}"
                        ),
                    });
                }
                expected_fragment += 1;

                if response.len() < 4 {
                    return Err(EdbgAvrError::UnexpectedResponse {
                        context: "EDBG receive",
                        details: format!(
                            "truncated fragment header: got {} bytes, need 4",
                            response.len()
                        ),
                    });
                }
                let claimed_len = u16::from_be_bytes([response[2], response[3]]) as usize;
                let available_len = response.len().saturating_sub(4);
                if available_len < claimed_len {
                    return Err(EdbgAvrError::UnexpectedResponse {
                        context: "EDBG receive",
                        details: format!(
                            "short fragment: claimed {claimed_len} bytes, got {available_len}"
                        ),
                    });
                }
                collected.extend_from_slice(&response[4..4 + claimed_len]);

                if fragment_number == total_fragments {
                    break;
                }
            }

            if collected.len() < 4 || collected[0] != TOKEN {
                return Err(EdbgAvrError::UnexpectedResponse {
                    context: "EDBG receive",
                    details: format!("malformed response {:02x?}", collected),
                });
            }

            let received_sequence = u16::from_le_bytes([collected[1], collected[2]]);
            if received_sequence != self.command_sequence {
                retries += 1;
                if retries >= MAX_SEQUENCE_RETRIES {
                    return Err(EdbgAvrError::UnexpectedResponse {
                        context: "EDBG receive",
                        details: format!(
                            "sequence mismatch after {MAX_SEQUENCE_RETRIES} retries \
                             (expected {}, got {received_sequence})",
                            self.command_sequence
                        ),
                    });
                }
                continue;
            }

            self.command_sequence = if self.command_sequence == 0xfffe {
                0
            } else {
                self.command_sequence + 1
            };

            return Ok(collected[3..].to_vec());
        }
    }

    fn transfer(&mut self, payload: &[u8]) -> Result<Vec<u8>, SendError> {
        let packet_size = self.packet_size();
        let buffer_len = packet_size + 1;
        let mut tx = vec![0u8; buffer_len];
        tx[1..1 + payload.len()].copy_from_slice(payload);

        let write_len = match self.device.as_ref() {
            #[cfg(feature = "cmsisdap_v1")]
            CmsisDapDevice::V1 { .. } => buffer_len,
            CmsisDapDevice::V2 { .. } => payload.len() + 1,
        };
        let _ = self.device.as_mut().write(&tx[..write_len])?;

        let mut rx = vec![0u8; buffer_len];
        let read_len = self.device.as_mut().read(&mut rx)?;
        rx.truncate(read_len);
        Ok(rx)
    }

    fn packet_size(&self) -> usize {
        match self.device.as_ref() {
            #[cfg(feature = "cmsisdap_v1")]
            CmsisDapDevice::V1 { report_size, .. } => *report_size,
            CmsisDapDevice::V2 {
                max_packet_size, ..
            } => *max_packet_size,
        }
    }
}

/// Run cleanup on the transport and combine any cleanup error with the main result.
/// If both the main operation and cleanup fail, the main error takes priority and
/// the cleanup error is logged as a warning.
fn finish_transport<T>(
    transport: &mut EdbgAvrTransport<'_>,
    result: Result<T, EdbgAvrError>,
) -> Result<T, EdbgAvrError> {
    match transport.cleanup() {
        Ok(()) => result,
        Err(cleanup_err) => {
            if result.is_err() {
                tracing::warn!("EDBG AVR transport cleanup also failed: {cleanup_err}");
                result
            } else {
                Err(cleanup_err)
            }
        }
    }
}

fn trim_probe_string(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim_end_matches('\0').to_string())
        .filter(|value| !value.is_empty())
}

fn trim_ascii_nul(mut bytes: Vec<u8>) -> String {
    while matches!(bytes.last(), Some(0)) {
        let _ = bytes.pop();
    }
    String::from_utf8_lossy(&bytes).trim_end().to_string()
}

fn partial_family_id_from_response(response: &[u8]) -> Option<String> {
    (response.len() >= 7 && response[1] == RSP3_DATA)
        .then(|| String::from_utf8_lossy(&response[3..7]).into_owned())
        .filter(|family| !family.trim_end_matches('\0').is_empty())
        .map(|family| family.trim_end_matches('\0').to_string())
}

fn build_updi_device_descriptor(chip: &AvrChipDescriptor) -> [u8; 48] {
    let mut descriptor = [0u8; 48];

    descriptor[0..2].copy_from_slice(&((chip.flash_base & 0xFFFF) as u16).to_le_bytes());
    descriptor[2] = (chip.flash_page_size & 0xFF) as u8;
    descriptor[3] = chip.eeprom_page_size;
    descriptor[4..6].copy_from_slice(&chip.nvm_base.to_le_bytes());
    descriptor[6..8].copy_from_slice(&chip.ocd_base.to_le_bytes());
    descriptor[8..10].copy_from_slice(&DEFAULT_MINIMUM_CHARACTERISED_DIV1_VOLTAGE_MV.to_le_bytes());
    descriptor[10..12]
        .copy_from_slice(&DEFAULT_MINIMUM_CHARACTERISED_DIV2_VOLTAGE_MV.to_le_bytes());
    descriptor[12..14]
        .copy_from_slice(&DEFAULT_MINIMUM_CHARACTERISED_DIV4_VOLTAGE_MV.to_le_bytes());
    descriptor[14..16]
        .copy_from_slice(&DEFAULT_MINIMUM_CHARACTERISED_DIV8_VOLTAGE_MV.to_le_bytes());
    descriptor[16..18].copy_from_slice(&MAX_FREQUENCY_SHARED_UPDI_PIN.to_le_bytes());
    descriptor[18..22].copy_from_slice(&chip.flash_size.to_le_bytes());
    descriptor[22..24].copy_from_slice(&(chip.eeprom_size as u16).to_le_bytes());
    descriptor[24..26].copy_from_slice(&(chip.userrow_size as u16).to_le_bytes());
    descriptor[26] = chip.fuses_size as u8;
    descriptor[27] = FUSES_SYSCFG0_OFFSET;
    descriptor[28] = 0xff;
    descriptor[29] = 0x00;
    descriptor[30] = 0xff;
    descriptor[31] = 0x00;
    descriptor[32..34].copy_from_slice(&(chip.eeprom_base as u16).to_le_bytes());
    descriptor[34..36].copy_from_slice(&(chip.userrow_base as u16).to_le_bytes());
    descriptor[36..38].copy_from_slice(&(chip.signature_base as u16).to_le_bytes());
    descriptor[38..40].copy_from_slice(&(chip.fuses_base as u16).to_le_bytes());
    descriptor[40..42].copy_from_slice(&(chip.lock_base as u16).to_le_bytes());
    descriptor[42] = chip.signature[1];
    descriptor[43] = chip.signature[2];
    descriptor[44] = (chip.flash_base >> 16) as u8;
    descriptor[45] = (chip.flash_page_size >> 8) as u8;
    descriptor[46] = chip.address_mode;
    descriptor[47] = chip.hvupdi_variant;

    descriptor
}

#[cfg(test)]
mod tests {
    use super::{ATMEGA4809, AVR64EA48, AVR128DB48, build_updi_device_descriptor};

    #[test]
    fn m4809_descriptor_matches_expected_layout() {
        let descriptor = build_updi_device_descriptor(&ATMEGA4809);

        assert_eq!(descriptor.len(), 48);
        assert_eq!(&descriptor[0..2], &0x4000u16.to_le_bytes());
        assert_eq!(descriptor[2], 128);
        assert_eq!(descriptor[3], 64);
        assert_eq!(&descriptor[4..6], &0x1000u16.to_le_bytes());
        assert_eq!(&descriptor[6..8], &0x0f80u16.to_le_bytes());
        assert_eq!(&descriptor[36..38], &0x1100u16.to_le_bytes());
        assert_eq!(descriptor[42], ATMEGA4809.signature[1]);
        assert_eq!(descriptor[43], ATMEGA4809.signature[2]);
        assert_eq!(descriptor[44], 0); // prog_base_msb: 0x4000 >> 16 = 0
        assert_eq!(descriptor[45], 0); // flash_page_size_msb: 128 >> 8 = 0
        assert_eq!(descriptor[46], 0); // 16-bit address mode
        assert_eq!(descriptor[47], 1); // hvupdi_variant
    }

    #[test]
    fn avr128db48_descriptor_has_24bit_addressing() {
        let descriptor = build_updi_device_descriptor(&AVR128DB48);

        assert_eq!(descriptor.len(), 48);
        // flash_base low 16 bits: 0x800000 & 0xFFFF = 0x0000
        assert_eq!(&descriptor[0..2], &0x0000u16.to_le_bytes());
        // flash_page_size low byte: 512 & 0xFF = 0
        assert_eq!(descriptor[2], 0);
        assert_eq!(descriptor[3], 1); // eeprom_page_size
        // flash_size = 0x20000
        assert_eq!(&descriptor[18..22], &0x0002_0000u32.to_le_bytes());
        // eeprom_size = 512
        assert_eq!(&descriptor[22..24], &512u16.to_le_bytes());
        // userrow_size = 32
        assert_eq!(&descriptor[24..26], &32u16.to_le_bytes());
        // fuses_size = 16
        assert_eq!(descriptor[26], 16);
        assert_eq!(descriptor[42], AVR128DB48.signature[1]);
        assert_eq!(descriptor[43], AVR128DB48.signature[2]);
        // prog_base_msb: 0x800000 >> 16 = 0x80
        assert_eq!(descriptor[44], 0x80);
        // flash_page_size_msb: 512 >> 8 = 2
        assert_eq!(descriptor[45], 2);
        // 24-bit address mode
        assert_eq!(descriptor[46], 1);
        assert_eq!(descriptor[47], 1); // hvupdi_variant
    }

    #[test]
    fn avr64ea48_descriptor_key_fields() {
        let descriptor = build_updi_device_descriptor(&AVR64EA48);

        assert_eq!(descriptor.len(), 48);
        // flash_base low 16 bits: 0x800000 & 0xFFFF = 0x0000
        assert_eq!(&descriptor[0..2], &0x0000u16.to_le_bytes());
        // flash_page_size low byte: 128 & 0xFF = 128
        assert_eq!(descriptor[2], 128);
        assert_eq!(descriptor[3], 8); // eeprom_page_size
        // flash_size = 0x10000
        assert_eq!(&descriptor[18..22], &0x0001_0000u32.to_le_bytes());
        assert_eq!(descriptor[42], AVR64EA48.signature[1]);
        assert_eq!(descriptor[43], AVR64EA48.signature[2]);
        // prog_base_msb: 0x800000 >> 16 = 0x80
        assert_eq!(descriptor[44], 0x80);
        // flash_page_size_msb: 128 >> 8 = 0
        assert_eq!(descriptor[45], 0);
        // 24-bit address mode
        assert_eq!(descriptor[46], 1);
        assert_eq!(descriptor[47], 2); // hvupdi_variant
    }
}
