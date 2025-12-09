use crate::{
    Core, CoreInterface, MemoryInterface,
    architecture::{riscv::Riscv32, xtensa::Xtensa},
    semihosting::{
        SemihostingCommand, UnknownCommandDetails, WriteConsoleRequest, ZeroTerminatedString,
    },
};

#[derive(Debug)]
pub(super) struct EspFlashSizeDetector {
    /// The address of the SPI flash peripheral (`SPIMEM1`).
    pub spiflash_peripheral: u32,
}

impl EspFlashSizeDetector {
    pub fn detect_flash_size_esp32(
        &self,
        core: &mut Core<'_>,
    ) -> Result<Option<usize>, crate::Error> {
        tracing::info!("Detecting flash size");
        detect_flash_size_esp32(core, self.spiflash_peripheral)
    }

    pub fn detect_flash_size(&self, core: &mut Core<'_>) -> Result<Option<usize>, crate::Error> {
        tracing::info!("Detecting flash size");
        detect_flash_size(core, self.spiflash_peripheral)
    }
}

struct SpiRegisters {
    base: u32,
    cmd: u32,
    addr: u32,
    ctrl: u32,
    user: u32,
    user1: u32,
    user2: u32,
    miso_dlen: u32,
    data_buf_0: u32,
}

impl SpiRegisters {
    fn cmd(&self) -> u64 {
        self.base as u64 | self.cmd as u64
    }

    fn addr(&self) -> u64 {
        self.base as u64 | self.addr as u64
    }

    fn ctrl(&self) -> u64 {
        self.base as u64 | self.ctrl as u64
    }

    fn user(&self) -> u64 {
        self.base as u64 | self.user as u64
    }

    fn user1(&self) -> u64 {
        self.base as u64 | self.user1 as u64
    }

    fn user2(&self) -> u64 {
        self.base as u64 | self.user2 as u64
    }

    fn miso_dlen(&self) -> u64 {
        self.base as u64 | self.miso_dlen as u64
    }

    fn data_buf_0(&self) -> u64 {
        self.base as u64 | self.data_buf_0 as u64
    }
}

fn execute_flash_command_generic(
    interface: &mut impl MemoryInterface,
    regs: &SpiRegisters,
    command: u8,
    miso_bits: u32,
) -> Result<u32, crate::Error> {
    // Save registers
    let old_ctrl_reg = interface.read_word_32(regs.ctrl())?;
    let old_user_reg = interface.read_word_32(regs.user())?;
    let old_user1_reg = interface.read_word_32(regs.user1())?;

    // ctrl register
    const CTRL_WP: u32 = 1 << 21;

    // user register
    const USER_MISO: u32 = 1 << 28;
    const USER_COMMAND: u32 = 1 << 31;

    // user2 register
    const USER_COMMAND_BITLEN: u32 = 28;

    // miso dlen register
    const MISO_BITLEN: u32 = 0;

    // cmd register
    const USER_CMD: u32 = 1 << 18;

    interface.write_word_32(regs.ctrl(), old_ctrl_reg | CTRL_WP)?;
    interface.write_word_32(regs.user(), old_user_reg | USER_COMMAND | USER_MISO)?;
    interface.write_word_32(regs.user1(), 0)?;
    interface.write_word_32(regs.user2(), (7 << USER_COMMAND_BITLEN) | command as u32)?;
    interface.write_word_32(regs.addr(), 0)?;
    interface.write_word_32(
        regs.miso_dlen(),
        (miso_bits.saturating_sub(1)) << MISO_BITLEN,
    )?;
    interface.write_word_32(regs.data_buf_0(), 0)?;

    // Execute read
    interface.write_word_32(regs.cmd(), USER_CMD)?;
    while interface.read_word_32(regs.cmd())? & USER_CMD != 0 {}

    // Read result
    let value = interface.read_word_32(regs.data_buf_0())?;

    // Restore registers
    interface.write_word_32(regs.ctrl(), old_ctrl_reg)?;
    interface.write_word_32(regs.user(), old_user_reg)?;
    interface.write_word_32(regs.user1(), old_user1_reg)?;

    Ok(value)
}

fn detect_flash_size(
    core: &mut Core<'_>,
    spiflash_addr: u32,
) -> Result<Option<usize>, crate::Error> {
    const RDID: u8 = 0x9F;

    let value = execute_flash_command_generic(
        core,
        &SpiRegisters {
            base: spiflash_addr,
            cmd: 0x00,
            addr: 0x04,
            ctrl: 0x08,
            user: 0x18,
            user1: 0x1C,
            user2: 0x20,
            miso_dlen: 0x28,
            data_buf_0: 0x58,
        },
        RDID,
        24,
    )?;

    Ok(decode_flash_size(value))
}

fn detect_flash_size_esp32(
    core: &mut Core<'_>,
    spiflash_addr: u32,
) -> Result<Option<usize>, crate::Error> {
    const RDID: u8 = 0x9F;

    let value = execute_flash_command_generic(
        core,
        &SpiRegisters {
            base: spiflash_addr,
            cmd: 0x00,
            addr: 0x04,
            ctrl: 0x08,
            user: 0x1C,
            user1: 0x20,
            user2: 0x24,
            miso_dlen: 0x2C,
            data_buf_0: 0x80,
        },
        RDID,
        24,
    )?;

    Ok(decode_flash_size(value))
}

fn decode_flash_size(value: u32) -> Option<usize> {
    let [manufacturer, memory_type, capacity, _] = value.to_le_bytes();

    tracing::debug!(
        "Detected manufacturer = {:x} memory_type = {:x} capacity = {:x}",
        manufacturer,
        memory_type,
        capacity
    );

    match espflash::flasher::FlashSize::from_detected(capacity) {
        Ok(capacity) => {
            let capacity = capacity.size() as usize;

            tracing::info!("Detected flash capacity: {:x}", capacity);

            Some(capacity)
        }
        _ => {
            tracing::warn!("Unknown flash capacity byte: {:x}", capacity);
            None
        }
    }
}

pub(super) struct EspBreakpointHandler {}

impl EspBreakpointHandler {
    pub fn handle_riscv_idf_semihosting(
        arch: &mut Riscv32,
        details: UnknownCommandDetails,
    ) -> Result<Option<SemihostingCommand>, crate::Error> {
        match details.operation {
            0x103 => {
                // ESP_SEMIHOSTING_SYS_BREAKPOINT_SET. Can be either set or clear breakpoint, and
                // depending on the operation the parameter pointer points to 2 or 3 words.
                let set_breakpoint = arch.read_word_32(details.parameter as u64)?;
                if set_breakpoint != 0 {
                    let mut breakpoint_data = [0; 2];
                    arch.read_32(details.parameter as u64 + 4, &mut breakpoint_data)?;
                    let [breakpoint_number, address] = breakpoint_data;
                    arch.set_hw_breakpoint(breakpoint_number as usize, address as u64)?;
                } else {
                    let breakpoint_number = arch.read_word_32(details.parameter as u64 + 4)?;
                    arch.clear_hw_breakpoint(breakpoint_number as usize)?;
                }
                Ok(None)
            }
            0x116 => Self::read_panic_reason(arch, details.parameter),
            _ => Ok(Some(SemihostingCommand::Unknown(details))),
        }
    }
    pub fn handle_xtensa_idf_semihosting(
        arch: &mut Xtensa,
        details: UnknownCommandDetails,
    ) -> Result<Option<SemihostingCommand>, crate::Error> {
        match details.operation {
            0x116 => Self::read_panic_reason(arch, details.parameter),
            _ => Ok(Some(SemihostingCommand::Unknown(details))),
        }
    }

    /// Handles ESP_SEMIHOSTING_SYS_PANIC_REASON by turning it into a `WriteConsoleRequest` command.
    fn read_panic_reason(
        arch: &mut dyn CoreInterface,
        parameter: u32,
    ) -> Result<Option<SemihostingCommand>, crate::Error> {
        let mut buffer = [0; 2];
        arch.read_32(parameter as u64, &mut buffer)?;

        let [address, length] = buffer;

        Ok(Some(SemihostingCommand::WriteConsole(WriteConsoleRequest(
            ZeroTerminatedString {
                address,
                length: Some(length),
            },
        ))))
    }
}
