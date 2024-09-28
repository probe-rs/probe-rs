use std::time::Duration;

use probe_rs_target::Architecture;

use crate::{
    architecture::xtensa::arch::{
        instruction::{into_binary, Instruction},
        CpuRegister,
    },
    Core, MemoryInterface, Session,
};

#[derive(Debug)]
pub(super) struct EspFlashSizeDetector {
    /// The target information. We calculate the stack pointer from the first flash algorithm.
    pub stack_pointer: u32,

    /// The address of the SPI flash peripheral (`SPIMEM1`).
    pub spiflash_peripheral: u32,

    /// The address of the `esp_rom_spiflash_attach` ROM function.
    pub attach_fn: u32,

    /// RAM address that we may use to download some code.
    pub load_address: u32,
}

impl EspFlashSizeDetector {
    fn attach_flash(&self, session: &mut Session) -> Result<(), crate::Error> {
        let mut core = session.core(0)?;
        core.reset_and_halt(Duration::from_millis(500))?;

        // call esp_rom_spiflash_attach(0, false)
        if core.architecture() == Architecture::Xtensa {
            setup_call_to_attach_xtensa(
                &mut core,
                self.stack_pointer,
                self.load_address,
                self.attach_fn,
            )?;
        } else {
            setup_call_to_attach_riscv(&mut core, self.stack_pointer, self.attach_fn)?;
        }

        // Let it run
        core.run()?;
        core.wait_for_core_halted(Duration::from_millis(500))?;

        Ok(())
    }

    pub fn detect_flash_size_esp32(
        &self,
        session: &mut Session,
    ) -> Result<Option<usize>, crate::Error> {
        tracing::info!("Detecting flash size");
        self.attach_flash(session)?;

        tracing::info!("Flash attached");
        detect_flash_size_esp32(session, self.spiflash_peripheral)
    }

    pub fn detect_flash_size(&self, session: &mut Session) -> Result<Option<usize>, crate::Error> {
        tracing::info!("Detecting flash size");
        self.attach_flash(session)?;

        tracing::info!("Flash attached");
        detect_flash_size(session, self.spiflash_peripheral)
    }
}

fn setup_call_to_attach_riscv(
    core: &mut Core<'_>,
    stack_pointer: u32,
    attach_fn: u32,
) -> Result<(), crate::Error> {
    use crate::architecture::riscv::assembly;

    // Return to a breakpoint
    core.write_32(stack_pointer as u64, &[assembly::EBREAK, assembly::EBREAK])?;

    let regs = core.registers();
    core.write_core_reg(core.program_counter(), attach_fn as u64)?;
    core.write_core_reg(regs.argument_register(0), 0_u64)?;
    core.write_core_reg(regs.argument_register(1), 0_u64)?;
    core.write_core_reg(core.stack_pointer(), stack_pointer)?;
    core.write_core_reg(core.return_address(), stack_pointer as u64)?;

    core.debug_on_sw_breakpoint(true)?;

    Ok(())
}

fn setup_call_to_attach_xtensa(
    core: &mut Core<'_>,
    stack_pointer: u32,
    load_addr: u32,
    attach_fn: u32,
) -> Result<(), crate::Error> {
    let instructions = into_binary([
        Instruction::CallX8(CpuRegister::A4),
        // Set a breakpoint at the end of the code
        Instruction::Break(0, 0),
    ]);

    // Download code
    core.write_8(load_addr as u64, &instructions)?;

    // Set up processor state
    let regs = core.registers();
    core.write_core_reg(core.program_counter(), load_addr)?;
    core.write_core_reg(CpuRegister::A4, attach_fn)?;
    core.write_core_reg(regs.argument_register(0), 0_u64)?;
    core.write_core_reg(regs.argument_register(1), 0_u64)?;
    core.write_core_reg(core.stack_pointer(), stack_pointer)?;

    Ok(())
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
    session: &mut Session,
    spiflash_addr: u32,
) -> Result<Option<usize>, crate::Error> {
    const RDID: u8 = 0x9F;

    let value = execute_flash_command_generic(
        &mut session.core(0)?,
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
    session: &mut Session,
    spiflash_addr: u32,
) -> Result<Option<usize>, crate::Error> {
    const RDID: u8 = 0x9F;

    let value = execute_flash_command_generic(
        &mut session.core(0)?,
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
