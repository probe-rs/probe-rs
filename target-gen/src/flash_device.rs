use scroll::Pread;

#[derive(Clone, Debug)]
pub struct Sector {
    pub address: u32,
    pub size: u32,
}

impl Sector {
    const SECTOR_END: u32 = 0xFFFF_FFFF;

    pub fn new(data: &[u8]) -> Option<Self> {
        let size = data.pread(0).unwrap();
        let address = data.pread(4).unwrap();
        if size != Self::SECTOR_END && address != Self::SECTOR_END {
            Some(Self {
                address,
                size,
            })
        } else {
            None
        }
    }
}

// This struct takes 160 bytes + the size of all sectors at the end in the ELF binary.
// The data types of this struct represent the actual size they have in the C struct too!
#[derive(Clone, Debug)]
pub struct FlashDevice {
    pub driver_version: u16,
    pub name: String, // Max 128 bytes in size
    pub typ: u16,
    pub address_start: u32,
    pub device_size: u32,
    pub page_size: u32,
    pub _reserved: u32,
    pub erased_default_value: u8,
    //  _pad: u24,
    pub program_page_timeout: u32, // in miliseconds
    pub erase_sector_timeout: u32, // in miliseconds
    pub sectors: Vec<Sector>,
}

impl FlashDevice {
    const INFO_SIZE: u32 = 160;
    const SECTOR_INFO_SIZE: u32 = 8;
    const MAX_ID_STRING_LENGTH: usize = 128;
    /// Parses the FlashDevice struct from ELF binary data.
    pub fn new(elf: &goblin::elf::Elf<'_>, buffer: &[u8], address: u32) -> Self {
        let mut sectors = vec![];

        let mut offset = Self::INFO_SIZE;
        while let Some(data) = crate::parser::read_elf_bin_data(elf, buffer, address + offset, Self::SECTOR_INFO_SIZE) {
            if let Some(sector) = Sector::new(data) {
                sectors.push(sector);
                offset += 8;
            } else {
                break;
            }
        }

        let data = crate::parser::read_elf_bin_data(elf, buffer, address, Self::INFO_SIZE).unwrap();

        let hypothetical_length = data[2..2 + Self::MAX_ID_STRING_LENGTH].iter().position(|&c| c == 0).unwrap_or(Self::MAX_ID_STRING_LENGTH);
        let sanitized_length = Self::MAX_ID_STRING_LENGTH.min(hypothetical_length);

        Self {
            driver_version: data.pread(0).unwrap(),
            name: String::from_utf8_lossy(&data[2..2 + sanitized_length]).to_string(),
            typ: data.pread(130).unwrap(),
            address_start: data.pread(132).unwrap(),
            device_size: data.pread(136).unwrap(),
            page_size: data.pread(140).unwrap(),
            _reserved: data.pread(144).unwrap(),
            erased_default_value: data.pread(148).unwrap(),
            program_page_timeout: data.pread(152).unwrap(),
            erase_sector_timeout: data.pread(156).unwrap(),
            sectors,
        }
    }
}