pub mod instruction;

// Register addresses

// Processor registers
pub const A3: u8 = 3;

// Special registers
pub const SR_DDR: u8 = 104;
pub const SR_EXCCAUSE: u8 = 232;
pub const SR_DEBUGCAUSE: u8 = 233;
pub const SR_EXCVADDR: u8 = 238;
