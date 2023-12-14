use probe_rs_target::{Chip, MemoryRegion};

use crate::{
    architecture::riscv::{
        assembly,
        communication_interface::{
            AccessRegisterCommand, RiscvBusAccess, RiscvCommunicationInterface,
        },
        registers::SP,
    },
    MemoryInterface,
};

#[derive(Debug)]
pub(super) struct EspFlashSizeDetector {
    /// The target information. We calculate the stack pointer from the first flash algorithm.
    pub stack_pointer: u32,

    /// The address of the SPI flash peripheral (`SPIMEM1`).
    pub spiflash_peripheral: u32,

    /// The address of the `esp_rom_spiflash_attach` ROM function.
    pub attach_fn: u32,
}

impl EspFlashSizeDetector {
    pub fn stack_pointer(chip: &Chip) -> u32 {
        chip.memory_map
            .iter()
            .find_map(|m| {
                if let MemoryRegion::Ram(ram) = m {
                    Some(ram.range.start as u32 + 0x1_0000)
                } else {
                    None
                }
            })
            .unwrap()
    }

    pub fn detect_flash_size(
        &self,
        interface: &mut RiscvCommunicationInterface,
    ) -> Result<Option<usize>, crate::Error> {
        esp32_attach_flash(interface, self.stack_pointer, self.attach_fn)?;
        esp32_detect_flash_size(interface, self.spiflash_peripheral)
    }
}

// Note that for the original ESP32 the process is slightly different.
fn esp32_execute_flash_command(
    interface: &mut RiscvCommunicationInterface,
    spiflash_addr: u32,
    command: u8,
    miso_bits: u32,
) -> Result<u32, crate::Error> {
    const CMD: u64 = 0x00;
    const CTRL: u64 = 0x08;
    const USER: u64 = 0x18;
    const USER1: u64 = 0x1C;
    const USER2: u64 = 0x20;
    const MISO_DLEN: u64 = 0x28;
    const DATA_BUF_0: u64 = 0x58;

    let base = spiflash_addr as u64;

    // Save registers
    let old_ctrl_reg = interface.read_word_32(base | CTRL)?;
    let old_user_reg = interface.read_word_32(base | USER)?;
    let old_user1_reg = interface.read_word_32(base | USER1)?;

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

    interface.write_word_32(base | CTRL, old_ctrl_reg | CTRL_WP)?;
    interface.write_word_32(base | USER, old_user_reg | USER_COMMAND | USER_MISO)?;
    interface.write_word_32(base | USER1, 0)?;
    interface.write_word_32(base | USER2, (7 << USER_COMMAND_BITLEN) | command as u32)?;
    interface.write_word_32(
        base | MISO_DLEN,
        (miso_bits.saturating_sub(1)) << MISO_BITLEN,
    )?;
    interface.write_word_32(base | DATA_BUF_0, 0)?;

    // Execute read
    interface.write_word_32(base | CMD, USER_CMD)?;
    while interface.read_word_32(base | CMD)? & USER_CMD != 0 {}

    // Read result
    let value = interface.read_word_32(base | DATA_BUF_0)?;

    // Restore registers
    interface.write_word_32(base | CTRL, old_ctrl_reg)?;
    interface.write_word_32(base | USER, old_user_reg)?;
    interface.write_word_32(base | USER1, old_user1_reg)?;

    Ok(value)
}

fn esp32_attach_flash(
    interface: &mut RiscvCommunicationInterface,
    stack_pointer: u32,
    attach_fn: u32,
) -> Result<(), crate::Error> {
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

fn esp32_detect_flash_size(
    interface: &mut RiscvCommunicationInterface,
    spiflash_addr: u32,
) -> Result<Option<usize>, crate::Error> {
    const RDID: u8 = 0x9F;
    let value = esp32_execute_flash_command(interface, spiflash_addr, RDID, 24)?;

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

    const KB: usize = 1024;
    const MB: usize = 1024 * KB;

    // TODO: replace with `espflash::flasher::FlashSize::from_detected` when
    // https://github.com/esp-rs/espflash/pull/530 gets released.
    let capacity = match (manufacturer, memory_type, capacity) {
        (_, _, 0x12) => 256 * KB,
        (_, _, 0x13) => 512 * KB,
        (_, _, 0x14) => MB,
        (_, _, 0x15) => 2 * MB,
        (_, _, 0x16) => 4 * MB,
        (_, _, 0x17) => 8 * MB,
        (_, _, 0x18) => 16 * MB,
        (_, _, 0x19) => 32 * MB,
        (_, _, 0x1A) => 64 * MB,
        (_, _, 0x1B) => 128 * MB,
        (_, _, 0x1C) => 256 * MB,
        (_, _, 0x20) => 64 * MB,
        (_, _, 0x21) => 128 * MB,
        (_, _, 0x22) => 256 * MB,
        (_, _, 0x32) => 256 * KB,
        (_, _, 0x33) => 512 * KB,
        (_, _, 0x34) => MB,
        (_, _, 0x35) => 2 * MB,
        (_, _, 0x36) => 4 * MB,
        (_, _, 0x37) => 8 * MB,
        (_, _, 0x38) => 16 * MB,
        (_, _, 0x39) => 32 * MB,
        (_, _, 0x3A) => 64 * MB,
        _ => return None,
    };
    tracing::debug!("Memory capacity = {:x}", capacity);

    Some(capacity)
}
