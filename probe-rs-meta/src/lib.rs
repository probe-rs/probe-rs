#![no_std]
#![doc = include_str!("../README.md")]
#![warn(missing_docs)]

/// Set the probe-rs chip.
///
/// ```rust
/// probe_rs_meta::chip!(b"rpi-pico");
/// ```
///
/// Note that you MUST use binary strings `b""`. Regular strings `""` will not work.
#[macro_export]
macro_rules! chip {
    ($val:literal) => {
        #[link_section = ".probe-rs.chip"]
        #[used]
        #[no_mangle] // prevent invoking the macro multiple times
        static _PROBE_RS_CHIP: [u8; $val.len()] = *$val;
    };
}

/// Set the probe-rs timeout, in seconds.
///
/// ```rust
/// probe_rs_meta::timeout!(60);
/// ```
#[macro_export]
macro_rules! timeout {
    ($val:literal) => {
        #[link_section = ".probe-rs.timeout"]
        #[used]
        #[no_mangle] // prevent invoking the macro multiple times
        static _PROBE_RS_TIMEOUT: u32 = $val;
    };
}
