use crate::debug_probe::DebugProbeError;
use coresight::access_ports::AccessPortError;
use ::arm_memory::MI;
use crate::session::Session;

use super::*;

const ANALYZER: [u32; 49] = [
    0x2780b5f0, 0x25004684, 0x4e2b2401, 0x447e4a2b, 0x0023007f, 0x425b402b, 0x40130868, 0x08584043,
    0x425b4023, 0x40584013, 0x40200843, 0x40104240, 0x08434058, 0x42404020, 0x40584010, 0x40200843,
    0x40104240, 0x08434058, 0x42404020, 0x40584010, 0x40200843, 0x40104240, 0x08584043, 0x425b4023,
    0x40434013, 0xc6083501, 0xd1d242bd, 0xd01f2900, 0x46602301, 0x469c25ff, 0x00894e11, 0x447e1841,
    0x88034667, 0x409f8844, 0x2f00409c, 0x2201d012, 0x4252193f, 0x34017823, 0x402b4053, 0x599b009b,
    0x405a0a12, 0xd1f542bc, 0xc00443d2, 0xd1e74281, 0xbdf02000, 0xe7f82200, 0x000000b2, 0xedb88320,
    0x00000042,
];

#[derive(Debug, Default, Copy, Clone)]
pub struct FlashAlgorithm {
    /// Memory address where the flash algo instructions will be loaded to.
    pub load_address: u32,
    /// List of 32-bit words containing the position-independant code for the algo.
    pub instructions: &'static [u32],
    /// Address of the `Init()` entry point. Optional.
    pub pc_init: Option<u32>,
    /// Address of the `UnInit()` entry point. Optional.
    pub pc_uninit: Option<u32>,
    /// Address of the `ProgramPage()` entry point.
    pub pc_program_page: u32,
    /// Address of the `EraseSector()` entry point.
    pub pc_erase_sector: u32,
    /// Address of the `EraseAll()` entry point. Optional.
    pub pc_erase_all: Option<u32>,
    /// Initial value of the R9 register for calling flash algo entry points, which
    /// determines where the position-independant data resides.
    pub static_base: u32,
    /// Initial value of the stack pointer when calling any flash algo API.
    pub begin_stack: u32,
    /// Base address of the page buffer. Used if `page_buffers` is not provided.
    pub begin_data: u32,
    /// An optional list of base addresses for page buffers. The buffers must be at
    /// least as large as the region's page_size attribute. If at least 2 buffers are included in
    /// the list, then double buffered programming will be enabled.
    pub page_buffers: &'static [u32],
    pub min_program_length: Option<u32>,
    /// Whether the CRC32-based analyzer is supported.
    pub analyzer_supported: bool,
    /// RAM base address where the analyzer code will be placed. There must be at
    /// least 0x600 free bytes after this address.
    pub analyzer_address: u32,
}

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
    fn operation() -> u32 { 1 }
}

pub struct Program;

impl Operation for Program {
    fn operation() -> u32 { 2 }
}

pub struct Verify;

impl Operation for Verify {
    fn operation() -> u32 { 3 }
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
    session: &'a mut Session,
    region: &'a FlashRegion,
    double_buffering_supported: bool
}

impl<'a> Flasher<'a> {
    pub fn new(session: &'a mut Session, region: &'a FlashRegion) -> Self {
        Self {
            session,
            region,
            double_buffering_supported: false,
        }
    }

    pub fn region(&self) -> &FlashRegion {
        &self.region
    }

    pub fn flash_algorithm(&self) -> &FlashAlgorithm {
        &self.session.target.flash_algorithm
    }

    pub fn double_buffering_supported(&self) -> bool {
        self.double_buffering_supported
    }

    pub fn init<'b, 's: 'b, O: Operation>(
        &'s mut self,
        mut address: Option<u32>,
        clock: Option<u32>
    ) -> Result<ActiveFlasher<'b, O>, FlasherError> {
        log::debug!("Initializing the flash algorithm.");
        let algo = self.session.target.flash_algorithm;

        use capstone::arch::*;
        let mut cs = capstone::Capstone::new()
            .arm()
            .mode(arm::ArchMode::Thumb)
            .endian(capstone::Endian::Little)
            .build()
            .unwrap();
        let i = algo.instructions
            .iter()
            .map(|i| [*i as u8, (*i >> 8) as u8, (*i >> 16) as u8, (*i >> 24) as u8])
            .collect::<Vec<[u8; 4]>>()
            .iter()
            .flatten()
            .map(|i| *i)
            .collect::<Vec<u8>>();

        let instructions = cs.disasm_all(i.as_slice(), algo.load_address as u64).unwrap();

        for instruction in instructions.iter() {
            log::debug!("{}", instruction);
        }

        if address.is_none() {
            address = Some(self.region.get_flash_info(algo.analyzer_supported).rom_start);
        }

        // TODO: Halt & reset target.
        log::debug!("Halting core.");
        let cpu_info = self.session.target.core.halt(&mut self.session.probe);
        log::debug!("PC = 0x{:08x}", cpu_info.unwrap().pc);

        // TODO: Possible special preparation of the target such as enabling faster clocks for the flash e.g.

        // Load flash algorithm code into target RAM.
        log::debug!("Loading algorithm into RAM at address 0x{:08x}", algo.load_address);
        self.session.probe.write_block32(algo.load_address, algo.instructions)?;

        let mut data = vec![0; algo.instructions.len()];
        self.session.probe.read_block32(algo.load_address, &mut data)?;

        assert_eq!(&algo.instructions, &data.as_slice());
        log::debug!("RAM contents match flashing algo blob.");

        log::debug!("Preparing Flasher for region:");
        log::debug!("{:#?}", &self.region);
        log::debug!("Double buffering enabled: {}", self.double_buffering_supported);
        let mut flasher = ActiveFlasher {
            session: self.session,
            region: self.region,
            double_buffering_supported: self.double_buffering_supported,
            _operation: core::marker::PhantomData,
        };

        // Execute init routine if one is present.
        if let Some(pc_init) = algo.pc_init {
            log::debug!("Running init routine.");
            let result = flasher.call_function_and_wait(
                pc_init,
                address,
                clock.or(Some(0)),
                Some(O::operation()),
                None,
                true
            )?;

            if result != 0 {
                return Err(FlasherError::Init(result));
            }
        }

        Ok(flasher)
    }

    pub fn run_erase<T, E: From<FlasherError>>(
        &mut self,
        f: impl FnOnce(&mut ActiveFlasher<Erase>) -> Result<T, E> + Sized
    ) -> Result<T, E> {
        // TODO: Fix those values (None, None).
        let mut active = self.init(None, None)?;
        let r = f(&mut active)?;
        active.uninit()?;
        Ok(r)
    }

    pub fn run_program<T, E: From<FlasherError>>(
        &mut self,
        f: impl FnOnce(&mut ActiveFlasher<Program>) -> Result<T, E> + Sized
    ) -> Result<T, E> {
        // TODO: Fix those values (None, None).
        let mut active = self.init(None, None)?;
        let r = f(&mut active)?;
        active.uninit()?;
        Ok(r)
    }

    pub fn run_verify<T, E: From<FlasherError>>(
        &mut self,
        f: impl FnOnce(&mut ActiveFlasher<Verify>) -> Result<T, E> + Sized
    ) -> Result<T, E> {
        // TODO: Fix those values (None, None).
        let mut active = self.init(None, None)?;
        let r = f(&mut active)?;
        active.uninit()?;
        Ok(r)
    }

    pub fn flash_block(
        mut self,
        address: u32,
        data: &[u8],
        chip_erase: Option<bool>,
        smart_flash: bool,
        fast_verify: bool,
    ) -> Result<(), FlasherError> {
        if !self.region.range.contains_range(&(address..address + data.len() as u32)) {
            return Err(FlasherError::AddressNotInRegion(address, self.region.clone()));
        }

        let mut fb = FlashBuilder::new(self.region.range.start);
        fb.add_data(address, data).expect("Add Data failed");
        fb.program(self, chip_erase, smart_flash, fast_verify, true).expect("Add Data failed");

        Ok(())
    }
}

pub struct ActiveFlasher<'a, O: Operation> {
    session: &'a mut Session,
    region: &'a FlashRegion,
    double_buffering_supported: bool,
    _operation: core::marker::PhantomData<O>,
}

impl<'a, O: Operation> ActiveFlasher<'a, O> {
    pub fn region(&self) -> &FlashRegion {
        &self.region
    }

    pub fn flash_algorithm(&self) -> &FlashAlgorithm {
        &self.session.target.flash_algorithm
    }

    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    pub fn uninit<'b, 's: 'b>(&'s mut self) -> Result<Flasher<'b>, FlasherError> {
        log::debug!("Running uninit routine.");
        let algo = self.session.target.flash_algorithm;

        if let Some(pc_uninit) = algo.pc_uninit {
            let result = self.call_function_and_wait(
                pc_uninit,
                Some(O::operation()),
                None,
                None,
                None,
                false
            )?;

            if result != 0 {
                return Err(FlasherError::Uninit(result));
            }
        }

        Ok(Flasher {
            session: self.session,
            region: self.region,
            double_buffering_supported: self.double_buffering_supported,
        })
    }

    fn call_function_and_wait(&mut self, pc: u32, r0: Option<u32>, r1: Option<u32>, r2: Option<u32>, r3: Option<u32>, init: bool) -> Result<u32, FlasherError> {
        self.call_function(pc, r0, r1, r2, r3, init)?;
        self.wait_for_completion()
    }

    fn call_function(&mut self, pc: u32, r0: Option<u32>, r1: Option<u32>, r2: Option<u32>, r3: Option<u32>, init: bool) -> Result<(), FlasherError> {
        log::debug!("Calling routine {:08x}({:?}, {:?}, {:?}, {:?}, init={})", pc, r0, r1, r2, r3, init);
        
        let algo = self.session.target.flash_algorithm;
        let regs = self.session.target.basic_register_addresses;

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
        .into_iter()
        .map(|(addr, value)| if let Some(v) = value {
            let r = self.session.target.core.write_core_reg(&mut self.session.probe, *addr, *v)?;
            log::debug!("content: 0x{:08x} should be: 0x{:08x}", self.session.target.core.read_core_reg(&mut self.session.probe, *addr)?, *v);
            Ok(r)
        } else {
            Ok(())
        })
        .collect::<Result<Vec<()>, DebugProbeError>>()?;

        // Resume target operation.
        self.session.target.core.run(&mut self.session.probe)?;

        Ok(())
    }

    pub fn wait_for_completion(&mut self) -> Result<u32, FlasherError> {
        log::debug!("Waiting for routine call completion.");
        let regs = self.session.target.basic_register_addresses;

        while self.session.target.core.wait_for_core_halted(&mut self.session.probe).is_err() {}

        let r = self.session.target.core.read_core_reg(&mut self.session.probe, regs.R0)?;
        Ok(r)
    }

    pub fn read_block32(&mut self, address: u32, data: &mut [u32]) -> Result<(), FlasherError> {
        self.session.probe.read_block32(address, data)?;
        Ok(())
    }

    pub fn read_block8(&mut self, address: u32, data: &mut [u8]) -> Result<(), FlasherError> {
        self.session.probe.read_block8(address, data)?;
        Ok(())
    }
}

impl <'a> ActiveFlasher<'a, Erase> {
    pub fn erase_all(&mut self) -> Result<(), FlasherError> {
        let algo = self.session.target.flash_algorithm;

        if let Some(pc_erase_all) = algo.pc_erase_all {
            let result = self.call_function_and_wait(
                pc_erase_all,
                None,
                None,
                None,
                None,
                false
            )?;

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
        let algo = self.session.target.flash_algorithm;

        let result = self.call_function_and_wait(
            algo.pc_erase_sector,
            Some(address),
            None,
            None,
            None,
            false
        )?;
        log::debug!("Done erasing sector. Result is {}", result);

        if result != 0 {
            Err(FlasherError::EraseSector(result, address))
        } else {
            Ok(())
        }
    }

    pub fn compute_crcs(&mut self, sectors: &Vec<(u32, u32)>) -> Result<Vec<u32>, FlasherError> {
        let algo = self.session.target.flash_algorithm;
        if algo.analyzer_supported {
            let mut data = vec![];

            self.session.probe.write_block32(algo.analyzer_address, &ANALYZER)?;

            for (address, mut size) in sectors {
                let size_value = {
                    let mut ndx = 0;
                    while 1 < size {
                        size = size >> 1;
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
                let value = (size_value << 0) | (address_value << 16);
                data.push(value);
            }

            self.session.probe.write_block32(algo.begin_data, data.as_slice())?;

            let result = self.call_function_and_wait(
                algo.analyzer_address,
                Some(algo.begin_data),
                Some(data.len() as u32),
                None,
                None,
                false
            );
            result?;

            self.session.probe.read_block32(algo.begin_data, data.as_mut_slice())?;

            Ok(data)
        } else {
            Err(FlasherError::AnalyzerNotSupported)
        }
    }
}

impl <'a> ActiveFlasher<'a, Program> {
    pub fn program_page(&mut self, address: u32, bytes: &[u8]) -> Result<(), FlasherError> {
        let algo = self.session.target.flash_algorithm;

        // TODO: Prevent security settings from locking the device.

        // Transfer the bytes to RAM.
        self.session.probe.write_block8(algo.begin_data, bytes)?;

        let result = self.call_function_and_wait(
            algo.pc_program_page,
            Some(address),
            Some(bytes.len() as u32),
            Some(algo.begin_data),
            None,
            false
        )?;

        if result != 0 {
            Err(FlasherError::ProgramPage(result, address))
        } else {
            Ok(())
        }
    }

    pub fn start_program_page_with_buffer(&mut self, address: u32, buffer_number: u32) -> Result<(), FlasherError> {
        let algo = self.session.target.flash_algorithm;

        // Check the buffer number.
        if buffer_number < algo.page_buffers.len() as u32 {
            return Err(FlasherError::InvalidBufferNumber(buffer_number, algo.page_buffers.len() as u32));
        }

        self.call_function(
            algo.pc_program_page,
            Some(address),
            Some(self.region.page_size),
            Some(algo.page_buffers[buffer_number as usize]),
            None,
            false
        )?;

        Ok(())
    }

    pub fn load_page_buffer(&mut self, _address: u32, bytes: &[u8], buffer_number: u32) -> Result<(), FlasherError> {
        let algo = self.session.target.flash_algorithm;

        // Check the buffer number.
        if buffer_number < algo.page_buffers.len() as u32 {
            return Err(FlasherError::InvalidBufferNumber(buffer_number, algo.page_buffers.len() as u32));
        }

        // TODO: Prevent security settings from locking the device.

        // Transfer the buffer bytes to RAM.
        self.session.probe.write_block8(algo.page_buffers[buffer_number as usize], bytes)?;

        Ok(())
    }

    pub fn program_phrase(&mut self, address: u32, bytes: &[u8]) -> Result<(), FlasherError> {
        let algo = self.session.target.flash_algorithm;

        // Get the minimum programming length. If none was specified, use the page size.
        let min_len = if let Some(min_program_length) = algo.min_program_length {
            min_program_length
        } else {
            self.region.page_size
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
        self.session.probe.write_block8(algo.begin_data, bytes)?;

        let result = self.call_function_and_wait(
            algo.pc_program_page,
            Some(address),
            Some(bytes.len() as u32),
            Some(algo.begin_data),
            None,
            false
        )?;

        if result != 0 {
            Err(FlasherError::ProgramPhrase(result, address))
        } else {
            Ok(())
        }
    }
}