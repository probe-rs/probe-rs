use crate::config::flash_algorithm::FlashAlgorithm;
use crate::coresight::access_ports::AccessPortError;
use crate::memory::MI;
use crate::probe::debug_probe::DebugProbeError;
use crate::probe::debug_probe::MasterProbe;
use crate::config::target::Target;

use crate::config::memory::{FlashRegion, MemoryRange};
use super::builder::FlashBuilder;

const ANALYZER: [u32; 49] = [
    0x2780_b5f0,
    0x2500_4684,
    0x4e2b_2401,
    0x447e_4a2b,
    0x0023_007f,
    0x425b_402b,
    0x4013_0868,
    0x0858_4043,
    0x425b_4023,
    0x4058_4013,
    0x4020_0843,
    0x4010_4240,
    0x0843_4058,
    0x4240_4020,
    0x4058_4010,
    0x4020_0843,
    0x4010_4240,
    0x0843_4058,
    0x4240_4020,
    0x4058_4010,
    0x4020_0843,
    0x4010_4240,
    0x0858_4043,
    0x425b_4023,
    0x4043_4013,
    0xc608_3501,
    0xd1d2_42bd,
    0xd01f_2900,
    0x4660_2301,
    0x469c_25ff,
    0x0089_4e11,
    0x447e_1841,
    0x8803_4667,
    0x409f_8844,
    0x2f00_409c,
    0x2201_d012,
    0x4252_193f,
    0x3401_7823,
    0x402b_4053,
    0x599b_009b,
    0x405a_0a12,
    0xd1f5_42bc,
    0xc004_43d2,
    0xd1e7_4281,
    0xbdf0_2000,
    0xe7f8_2200,
    0x0000_00b2,
    0xedb8_8320,
    0x0000_0042,
];

pub trait Operation {
    fn operation() -> u32;
    fn operation_name(&self) -> &str {
        match Self::operation() {
            1 => "Erase",
            2 => "Program",
            3 => "Verify",
            _ => "Unknown Operation",
        }
    }
}

pub struct Erase;

impl Operation for Erase {
    fn operation() -> u32 {
        1
    }
}

pub struct Program;

impl Operation for Program {
    fn operation() -> u32 {
        2
    }
}

pub struct Verify;

impl Operation for Verify {
    fn operation() -> u32 {
        3
    }
}

#[derive(Debug)]
pub enum FlasherError {
    Init(u32),
    Uninit(u32),
    EraseAll(u32),
    EraseAllNotSupported,
    EraseSector(u32, u32),
    ProgramPage(u32, u32),
    InvalidBufferNumber(u32, u32),
    UnalignedFlashWriteAddress,
    UnalignedPhraseLength,
    ProgramPhrase(u32, u32),
    AnalyzerNotSupported,
    SizeNotPowerOf2,
    AddressNotMultipleOfSize,
    AccessPort(AccessPortError),
    DebugProbe(DebugProbeError),
    AddressNotInRegion(u32, FlashRegion),
}

impl From<DebugProbeError> for FlasherError {
    fn from(error: DebugProbeError) -> FlasherError {
        FlasherError::DebugProbe(error)
    }
}

impl From<AccessPortError> for FlasherError {
    fn from(error: AccessPortError) -> FlasherError {
        FlasherError::AccessPort(error)
    }
}

pub struct Flasher<'a> {
    target: &'a Target,
    probe: &'a mut MasterProbe,
    flash_algorithm: &'a FlashAlgorithm,
    region: &'a FlashRegion,
    double_buffering_supported: bool,
}

impl<'a> Flasher<'a> {
    pub fn new(
        target: &'a Target,
        probe: &'a mut MasterProbe,
        flash_algorithm: &'a FlashAlgorithm,
        region: &'a FlashRegion,
    ) -> Self {
        Self {
            target,
            probe,
            flash_algorithm,
            region,
            double_buffering_supported: false,
        }
    }

    pub fn region(&self) -> &FlashRegion {
        &self.region
    }

    pub fn flash_algorithm(&self) -> &FlashAlgorithm {
        &self.flash_algorithm
    }

    pub fn double_buffering_supported(&self) -> bool {
        self.double_buffering_supported
    }

    pub fn init<'b, 's: 'b, O: Operation>(
        &'s mut self,
        mut address: Option<u32>,
        clock: Option<u32>,
    ) -> Result<ActiveFlasher<'b, O>, FlasherError> {
        log::debug!("Initializing the flash algorithm.");
        let flasher = self;
        let algo = flasher.flash_algorithm;

        use capstone::arch::*;
        let cs = capstone::Capstone::new()
            .arm()
            .mode(arm::ArchMode::Thumb)
            .endian(capstone::Endian::Little)
            .build()
            .unwrap();
        let i = algo
            .instructions
            .iter()
            .map(|i| {
                [
                    *i as u8,
                    (*i >> 8) as u8,
                    (*i >> 16) as u8,
                    (*i >> 24) as u8,
                ]
            })
            .collect::<Vec<[u8; 4]>>()
            .iter()
            .flatten()
            .copied()
            .collect::<Vec<u8>>();

        let instructions = cs
            .disasm_all(i.as_slice(), u64::from(algo.load_address))
            .unwrap();

        for instruction in instructions.iter() {
            log::debug!("{}", instruction);
        }

        if address.is_none() {
            address = Some(
                flasher
                    .region
                    .flash_info(algo.analyzer_supported)
                    .rom_start,
            );
        }

        // TODO: Halt & reset target.
        log::debug!("Halting core.");
        let cpu_info = flasher.target.core.halt(&mut flasher.probe);
        log::debug!("PC = 0x{:08x}", cpu_info.unwrap().pc);
        flasher
            .target
            .core
            .wait_for_core_halted(&mut flasher.probe)?;
        log::debug!("Reset and halt");
        flasher.target.core.reset_and_halt(&mut flasher.probe)?;

        // TODO: Possible special preparation of the target such as enabling faster clocks for the flash e.g.

        // Load flash algorithm code into target RAM.
        log::debug!(
            "Loading algorithm into RAM at address 0x{:08x}",
            algo.load_address
        );
        flasher
            .probe
            .write_block32(algo.load_address, algo.instructions.as_slice())?;

        let mut data = vec![0; algo.instructions.len()];
        flasher.probe.read_block32(algo.load_address, &mut data)?;

        assert_eq!(&algo.instructions, &data.as_slice());
        log::debug!("RAM contents match flashing algo blob.");

        log::debug!("Preparing Flasher for region:");
        log::debug!("{:#?}", &flasher.region);
        log::debug!(
            "Double buffering enabled: {}",
            flasher.double_buffering_supported
        );
        let mut flasher = ActiveFlasher {
            target: flasher.target,
            probe: flasher.probe,
            flash_algorithm: flasher.flash_algorithm,
            region: flasher.region,
            double_buffering_supported: flasher.double_buffering_supported,
            _operation: core::marker::PhantomData,
        };

        flasher.init(address, clock)?;

        Ok(flasher)
    }

    pub fn run_erase<T, E: From<FlasherError>>(
        &mut self,
        f: impl FnOnce(&mut ActiveFlasher<Erase>) -> Result<T, E> + Sized,
    ) -> Result<T, E> {
        // TODO: Fix those values (None, None).
        let mut active = self.init(None, None)?;
        let r = f(&mut active)?;
        active.uninit()?;
        Ok(r)
    }

    pub fn run_program<T, E: From<FlasherError>>(
        &mut self,
        f: impl FnOnce(&mut ActiveFlasher<Program>) -> Result<T, E> + Sized,
    ) -> Result<T, E> {
        // TODO: Fix those values (None, None).
        let mut active = self.init(None, None)?;
        let r = f(&mut active)?;
        active.uninit()?;
        Ok(r)
    }

    pub fn run_verify<T, E: From<FlasherError>>(
        &mut self,
        f: impl FnOnce(&mut ActiveFlasher<Verify>) -> Result<T, E> + Sized,
    ) -> Result<T, E> {
        // TODO: Fix those values (None, None).
        let mut active = self.init(None, None)?;
        let r = f(&mut active)?;
        active.uninit()?;
        Ok(r)
    }

    pub fn flash_block(
        self,
        address: u32,
        data: &[u8],
        chip_erase: Option<bool>,
        smart_flash: bool,
        _fast_verify: bool,
    ) -> Result<(), FlasherError> {
        if !self
            .region
            .range
            .contains_range(&(address..address + data.len() as u32))
        {
            return Err(FlasherError::AddressNotInRegion(
                address,
                self.region.clone(),
            ));
        }

        let mut fb = FlashBuilder::new(self.region.range.start);
        fb.add_data(address, data).expect("Add Data failed");
        fb.program(self, chip_erase, smart_flash, true)
            .expect("Add Data failed");

        Ok(())
    }
}

pub struct ActiveFlasher<'a, O: Operation> {
    target: &'a Target,
    probe: &'a mut MasterProbe,
    flash_algorithm: &'a FlashAlgorithm,
    region: &'a FlashRegion,
    double_buffering_supported: bool,
    _operation: core::marker::PhantomData<O>,
}

impl<'a, O: Operation> ActiveFlasher<'a, O> {
    pub fn init(&mut self, address: Option<u32>, clock: Option<u32>) -> Result<(), FlasherError> {
        let algo = &self.flash_algorithm;

        // Execute init routine if one is present.
        if let Some(pc_init) = algo.pc_init {
            log::debug!("Running init routine.");
            let result = self.call_function_and_wait(
                pc_init,
                address,
                clock.or(Some(0)),
                Some(O::operation()),
                None,
                true,
            )?;

            if result != 0 {
                return Err(FlasherError::Init(result));
            }
        }

        Ok(())
    }

    pub fn region(&self) -> &FlashRegion {
        &self.region
    }

    pub fn flash_algorithm(&self) -> &FlashAlgorithm {
        &&self.flash_algorithm
    }

    // pub fn session_mut(&mut self) -> &mut Session {
    //     &mut self.session
    // }

    pub fn uninit<'b, 's: 'b>(&'s mut self) -> Result<Flasher<'b>, FlasherError> {
        log::debug!("Running uninit routine.");
        let algo = &self.flash_algorithm;

        if let Some(pc_uninit) = algo.pc_uninit {
            let result = self.call_function_and_wait(
                pc_uninit,
                Some(O::operation()),
                None,
                None,
                None,
                false,
            )?;

            if result != 0 {
                return Err(FlasherError::Uninit(result));
            }
        }

        Ok(Flasher {
            target: self.target,
            probe: self.probe,
            flash_algorithm: self.flash_algorithm,
            region: self.region,
            double_buffering_supported: self.double_buffering_supported,
        })
    }

    fn call_function_and_wait(
        &mut self,
        pc: u32,
        r0: Option<u32>,
        r1: Option<u32>,
        r2: Option<u32>,
        r3: Option<u32>,
        init: bool,
    ) -> Result<u32, FlasherError> {
        self.call_function(pc, r0, r1, r2, r3, init)?;
        self.wait_for_completion()
    }

    fn call_function(
        &mut self,
        pc: u32,
        r0: Option<u32>,
        r1: Option<u32>,
        r2: Option<u32>,
        r3: Option<u32>,
        init: bool,
    ) -> Result<(), FlasherError> {
        log::debug!(
            "Calling routine {:08x}({:?}, {:?}, {:?}, {:?}, init={})",
            pc,
            r0,
            r1,
            r2,
            r3,
            init
        );

        let algo = &self.flash_algorithm;
        let regs = self.target.core.registers();

        [
            (regs.PC, Some(pc)),
            (regs.R0, r0),
            (regs.R1, r1),
            (regs.R2, r2),
            (regs.R3, r3),
            (regs.R9, if init { Some(algo.static_base) } else { None }),
            (regs.SP, if init { Some(algo.begin_stack) } else { None }),
            (regs.LR, Some(algo.load_address + 1)),
        ]
        .iter()
        .map(|(addr, value)| {
            if let Some(v) = value {
                self.target
                    .core
                    .write_core_reg(&mut self.probe, *addr, *v)?;
                log::debug!(
                    "content of {:#x}: 0x{:08x} should be: 0x{:08x}",
                    addr.0,
                    self.target.core.read_core_reg(&mut self.probe, *addr)?,
                    *v
                );
                Ok(())
            } else {
                Ok(())
            }
        })
        .collect::<Result<Vec<()>, DebugProbeError>>()?;

        // Resume target operation.
        self.target.core.run(&mut self.probe)?;

        Ok(())
    }

    pub fn wait_for_completion(&mut self) -> Result<u32, FlasherError> {
        log::debug!("Waiting for routine call completion.");
        let regs = self.target.core.registers();

        while self
            .target
            .core
            .wait_for_core_halted(&mut self.probe)
            .is_err()
        {}

        let r = self.target.core.read_core_reg(&mut self.probe, regs.R0)?;
        Ok(r)
    }

    pub fn read_block32(&mut self, address: u32, data: &mut [u32]) -> Result<(), FlasherError> {
        self.probe.read_block32(address, data)?;
        Ok(())
    }

    pub fn read_block8(&mut self, address: u32, data: &mut [u8]) -> Result<(), FlasherError> {
        self.probe.read_block8(address, data)?;
        Ok(())
    }
}

impl<'a> ActiveFlasher<'a, Erase> {
    pub fn erase_all(&mut self) -> Result<(), FlasherError> {
        let flasher = self;
        let algo = flasher.flash_algorithm;

        if let Some(pc_erase_all) = algo.pc_erase_all {
            let result =
                flasher.call_function_and_wait(pc_erase_all, None, None, None, None, false)?;

            if result != 0 {
                Err(FlasherError::EraseAll(result))
            } else {
                Ok(())
            }
        } else {
            Err(FlasherError::EraseAllNotSupported)
        }
    }

    pub fn erase_sector(&mut self, address: u32) -> Result<(), FlasherError> {
        log::debug!("Erasing sector at address 0x{:08x}.", address);
        let flasher = self;
        let algo = flasher.flash_algorithm;

        let result = flasher.call_function_and_wait(
            algo.pc_erase_sector,
            Some(address),
            None,
            None,
            None,
            false,
        )?;
        log::debug!("Done erasing sector. Result is {}", result);

        if result != 0 {
            Err(FlasherError::EraseSector(result, address))
        } else {
            Ok(())
        }
    }

    pub fn compute_crcs(&mut self, sectors: &[(u32, u32)]) -> Result<Vec<u32>, FlasherError> {
        let flasher = self;
        let algo = flasher.flash_algorithm;

        if algo.analyzer_supported {
            let mut data = vec![];

            flasher
                .probe
                .write_block32(algo.analyzer_address, &ANALYZER)?;

            for (address, mut size) in sectors {
                let size_value = {
                    let mut ndx = 0;
                    while 1 < size {
                        size >>= 1;
                        ndx += 1;
                    }
                    ndx
                };
                let address_value = address / size;
                if 1 << size_value != size {
                    return Err(FlasherError::SizeNotPowerOf2);
                }
                if address % size != 0 {
                    return Err(FlasherError::AddressNotMultipleOfSize);
                }
                let value = size_value | (address_value << 16);
                data.push(value);
            }

            flasher
                .probe
                .write_block32(algo.begin_data, data.as_slice())?;

            let analyzer_address = algo.analyzer_address;
            let begin_data = algo.begin_data;
            let result = flasher.call_function_and_wait(
                analyzer_address,
                Some(begin_data),
                Some(data.len() as u32),
                None,
                None,
                false,
            );
            result?;

            flasher
                .probe
                .read_block32(begin_data, data.as_mut_slice())?;

            Ok(data)
        } else {
            Err(FlasherError::AnalyzerNotSupported)
        }
    }
}

impl<'a> ActiveFlasher<'a, Program> {
    pub fn program_page(&mut self, address: u32, bytes: &[u8]) -> Result<(), FlasherError> {
        let flasher = self;
        let algo = flasher.flash_algorithm;

        // TODO: Prevent security settings from locking the device.

        // Transfer the bytes to RAM.
        flasher.probe.write_block8(algo.begin_data, bytes)?;

        let result = flasher.call_function_and_wait(
            algo.pc_program_page,
            Some(address),
            Some(bytes.len() as u32),
            Some(algo.begin_data),
            None,
            false,
        )?;

        if result != 0 {
            Err(FlasherError::ProgramPage(result, address))
        } else {
            Ok(())
        }
    }

    pub fn start_program_page_with_buffer(
        &mut self,
        address: u32,
        buffer_number: u32,
    ) -> Result<(), FlasherError> {
        let flasher = self;
        let algo = flasher.flash_algorithm;

        // Check the buffer number.
        if buffer_number < algo.page_buffers.len() as u32 {
            return Err(FlasherError::InvalidBufferNumber(
                buffer_number,
                algo.page_buffers.len() as u32,
            ));
        }

        flasher.call_function(
            algo.pc_program_page,
            Some(address),
            Some(flasher.region.page_size),
            Some(algo.page_buffers[buffer_number as usize]),
            None,
            false,
        )?;

        Ok(())
    }

    pub fn load_page_buffer(
        &mut self,
        _address: u32,
        bytes: &[u8],
        buffer_number: u32,
    ) -> Result<(), FlasherError> {
        let flasher = self;
        let algo = flasher.flash_algorithm;

        // Check the buffer number.
        if buffer_number < algo.page_buffers.len() as u32 {
            return Err(FlasherError::InvalidBufferNumber(
                buffer_number,
                algo.page_buffers.len() as u32,
            ));
        }

        // TODO: Prevent security settings from locking the device.

        // Transfer the buffer bytes to RAM.
        flasher
            .probe
            .write_block8(algo.page_buffers[buffer_number as usize], bytes)?;

        Ok(())
    }

    pub fn program_phrase(&mut self, address: u32, bytes: &[u8]) -> Result<(), FlasherError> {
        let flasher = self;
        let algo = flasher.flash_algorithm;

        // Get the minimum programming length. If none was specified, use the page size.
        let min_len = if let Some(min_program_length) = algo.min_program_length {
            min_program_length
        } else {
            flasher.region.page_size
        };

        // Require write address and length to be aligned to the minimum write size.
        if address % min_len != 0 {
            return Err(FlasherError::UnalignedFlashWriteAddress);
        }
        if bytes.len() as u32 % min_len != 0 {
            return Err(FlasherError::UnalignedPhraseLength);
        }

        // TODO: Prevent security settings from locking the device.

        // Transfer the phrase bytes to RAM.
        flasher.probe.write_block8(algo.begin_data, bytes)?;

        let result = flasher.call_function_and_wait(
            algo.pc_program_page,
            Some(address),
            Some(bytes.len() as u32),
            Some(algo.begin_data),
            None,
            false,
        )?;

        if result != 0 {
            Err(FlasherError::ProgramPhrase(result, address))
        } else {
            Ok(())
        }
    }
}
