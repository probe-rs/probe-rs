//! This crate provides a means to retrieve the JEDEC manufacturer string
//! for a corresponding JEP106 ID Code.
//!
//! All the codes can be found on the page of the JEDEC organization
//! but are presented in the riddiculous form of a PDF.
//! 
//! This crate parses the PDF and exposes an interface
//! to poll the JEDEC manufacturer string of a JEP106 ID code.
//! 
//! # Example
//!
//! ```
//! fn main() {
//!     let nordic = jep106::JEP106Code::new(0x02, 0x44).get();
//!     assert_eq!("Nordic VLSI ASA", nordic);
//! }
//! ```

/// A Struct which contains a fully qualified JEP106 manufacturer code.
#[derive(Copy, Clone, PartialEq, Eq)]
pub struct JEP106Code {
    /// JEP106 identification code.
    /// Points to a manufacturer name in the bank table corresponding to `cc`.
    pub id: u8,
    /// JEP106 continuation code.
    /// This code represents the bank which the manufacturer for a corresponding `id` has to be looked up.
    pub cc: u8,
}

impl JEP106Code {
    /// Creates a new [JEP106Code](struct.JEP106Code.html) struct from
    /// a JEP106 continuation code and a JEP106 id code tuple.
    pub const fn new(cc: u8, id: u8) -> Self {
        Self {
            id,
            cc,
        }
    }

    /// Returns the manufacturer corresponding to a complete JEP106 code.
    /// 
    /// Returns an empty string if the JEP106 code is unknown.
    pub const fn get(&self) -> &'static str {
        get(self.cc, self.id)
    }
    
    /// Returns the manufacturer corresponding to
    /// a JEP106 continuation code and a JEP106 id code tuple.
    /// 
    /// Returns an empty string if the JEP106 code is unknown.
    pub const fn get_raw(cc: u8, id: u8) -> &'static str {
        get(cc, id)
    }
}

impl std::fmt::Debug for JEP106Code {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "JEP106Code({{ cc: 0x{:02x?}, id: 0x{:02x?} }} => {})", self.cc, self.id, self.get())
    }
}

include!(concat!(env!("OUT_DIR"), "/jep106.rs"));