#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct RawFlashAlgorithm {
    /// The name of the flash algorithm.
    pub name: String,
    /// Whether this flash algorithm is the default one or not.
    pub default: bool,
    /// Memory address where the flash algo instructions will be loaded to.
    pub load_address: u32,
    /// List of 32-bit words containing the position-independent code for the algo.
    pub instructions: Vec<u32>,
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
    /// determines where the position-independent data resides.
    pub static_base: u32,
    /// Initial value of the stack pointer when calling any flash algo API.
    pub begin_stack: u32,
    /// Base address of the page buffer. Used if `page_buffers` is not provided.
    pub begin_data: u32,
    /// An optional list of base addresses for page buffers. The buffers must be at
    /// least as large as the region's `page_size` attribute. If at least 2 buffers are included in
    /// the list, then double buffered programming will be enabled.
    pub page_buffers: Vec<u32>,
    pub min_program_length: Option<u32>,
    /// Whether the CRC32-based analyzer is supported.
    pub analyzer_supported: bool,
    /// RAM base address where the analyzer code will be placed. There must be at
    /// least 0x600 free bytes after this address.
    pub analyzer_address: u32,
}
