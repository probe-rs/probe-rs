use std::time::Duration;

use probe_rs_target::{Architecture, Chip, MemoryRegion};

use crate::{
    architecture::xtensa::arch::{
        instruction::{into_binary, Instruction},
        CpuRegister, Register,
    },
    config::DebugSequence,
    MemoryInterface, Session,
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
    pub fn stack_pointer(chip: &Chip) -> u32 {
        chip.memory_map
            .iter()
            .find_map(MemoryRegion::as_ram_region)
            .map(|ram| ram.range.start as u32 + 0x1_0000)
            .unwrap()
    }

    pub fn detect_flash_size_esp32(
        &self,
        session: &mut Session,
    ) -> Result<Option<usize>, crate::Error> {
        tracing::info!("Detecting flash size");
        attach_flash_xtensa(
            session,
            self.stack_pointer,
            self.load_address,
            self.attach_fn,
        )?;

        tracing::info!("Flash attached");
        detect_flash_size_esp32(session, self.spiflash_peripheral)
    }

    pub fn detect_flash_size(&self, session: &mut Session) -> Result<Option<usize>, crate::Error> {
        tracing::info!("Detecting flash size");
        if session.target().architecture() == Architecture::Xtensa {
            attach_flash_xtensa(
                session,
                self.stack_pointer,
                self.load_address,
                self.attach_fn,
            )?;
        } else {
            attach_flash_riscv(session, self.stack_pointer, self.attach_fn)?;
        }

        tracing::info!("Flash attached");
        detect_flash_size(session, self.spiflash_peripheral)
    }
}

fn attach_flash_riscv(
    session: &mut Session,
    stack_pointer: u32,
    attach_fn: u32,
) -> Result<(), crate::Error> {
    use crate::architecture::riscv::{
        assembly,
        communication_interface::{AccessRegisterCommand, RiscvBusAccess},
        registers::SP,
    };

    let interface = &mut session.get_riscv_interface(0)?;
    interface.halt(Duration::from_millis(100))?;

    // Set a valid-ish stack pointer
    interface.abstract_cmd_register_write(SP, stack_pointer)?;

    // esp_rom_spiflash_attach(0, false)
    interface.schedule_setup_program_buffer(&[
        assembly::addi(0, 10, 0),                       // c.li a0, zero
        assembly::addi(0, 11, 0),                       // c.li a1, zero
        assembly::lui(5, (attach_fn >> 12) as i32),     // lui x5, ...
        assembly::jarl(5, 1, attach_fn as i32 & 0xFFF), // jarl ra, x5, ...
    ])?;

    // Actually call the function
    let mut execute_progbuf = AccessRegisterCommand(0);
    execute_progbuf.set_cmd_type(0);
    execute_progbuf.set_transfer(false);
    execute_progbuf.set_write(false);
    execute_progbuf.set_aarsize(RiscvBusAccess::A32);
    execute_progbuf.set_postexec(true);
    execute_progbuf.set_regno(SP.id.0 as u32);

    interface.schedule_write_dm_register(execute_progbuf)?;
    interface.execute()?;

    Ok(())
}

fn attach_flash_xtensa(
    session: &mut Session,
    stack_pointer: u32,
    load_addr: u32,
    attach_fn: u32,
) -> Result<(), crate::Error> {
    // TODO: we shouldn't need to touch sequences here.
    let DebugSequence::Xtensa(sequence) = session.target().debug_sequence.clone() else {
        unreachable!()
    };
    let interface = &mut session.get_xtensa_interface(0)?;

    // We're very intrusive here but the flashing process should reset the MCU again anyway
    sequence.reset_system_and_halt(interface, Duration::from_millis(500))?;

    let instructions = into_binary([
        Instruction::CallX8(CpuRegister::A4),
        // Set a breakpoint at the end of the code
        Instruction::Break(0, 0),
    ]);

    // Download code
    interface.write_8(load_addr as u64, &instructions)?;

    // Set up processor state
    interface.write_register_untyped(Register::CurrentPc, load_addr)?;

    interface.write_register_untyped(CpuRegister::A1, stack_pointer)?;
    interface.write_register_untyped(CpuRegister::A4, attach_fn)?;

    // Let it run
    tracing::debug!("Running program to attach flash");
    interface.resume()?;

    interface.wait_for_core_halted(Duration::from_millis(500))?;

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
