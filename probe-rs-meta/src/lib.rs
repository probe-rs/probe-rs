#![no_std]
#![doc = include_str!("../README.md")]
#![warn(missing_docs)]

const fn array<const N: usize>(slice: &[u8]) -> [u8; N] {
    // Ensure the slice length matches at compile time (will fail to compile otherwise)
    assert!(slice.len() == N, "Slice length must match array length");

    let mut arr = [0u8; N];
    let mut i = 0;
    while i < N {
        arr[i] = slice[i];
        i += 1;
    }
    arr
}

#[unsafe(link_section = ".probe-rs.version")]
#[used]
#[unsafe(no_mangle)] // prevent invoking the macro multiple times
static _PROBE_RS_META_VERSION: [u8; env!("CARGO_PKG_VERSION").as_bytes().len()] =
    array(env!("CARGO_PKG_VERSION").as_bytes());

/// Set the probe-rs chip.
///
/// ```rust
/// probe_rs_meta::chip!(b"rp2040");
/// ```
///
/// Note that you MUST use binary strings `b""`. Regular strings `""` will not work.
#[macro_export]
macro_rules! chip {
    ($val:literal) => {
        #[unsafe(link_section = ".probe-rs.chip")]
        #[used]
        #[unsafe(no_mangle)] // prevent invoking the macro multiple times
        static _PROBE_RS_CHIP: [u8; $val.len()] = *$val;
    };
}

/// Set the maximum time that this program should be able to run until a breakpoint or fault is encountered.
///
/// ```rust
/// probe_rs_meta::timeout!(60);
/// ```
#[macro_export]
macro_rules! timeout {
    ($val:literal) => {
        #[unsafe(link_section = ".probe-rs.timeout")]
        #[used]
        #[unsafe(no_mangle)] // prevent invoking the macro multiple times
        static _PROBE_RS_TIMEOUT: u32 = $val;
    };
}
