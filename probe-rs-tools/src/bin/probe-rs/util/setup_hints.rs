//! Hints shown to the user when a probe can't be found or accessed.
//!
//! These are a CLI concern: the library only reports what it found, and the CLI decides
//! whether to nudge the user about their setup.

/// Prints a setup hint when a probe is missing or can't be accessed.
///
/// Only does anything on Linux, and never when `PROBE_RS_DISABLE_SETUP_HINTS` is set.
pub fn print_setup_hints() {
    #[cfg(target_os = "linux")]
    linux::print_setup_hints();
}

#[cfg(target_os = "linux")]
mod linux {
    const SETUP_URL: &str = "https://probe.rs/docs/getting-started/probe-setup/";

    pub(super) fn print_setup_hints() {
        if std::env::var("PROBE_RS_DISABLE_SETUP_HINTS").is_ok() {
            return;
        }

        // We deliberately don't try to guess the cause (missing udev rule, wrong group,
        // ...): that guessing is wrong on NixOS, Alpine, serial probes, etc. Whatever the
        // cause, the fix is documented, so point there.
        tracing::warn!(
            "If your probe is plugged in but not listed, or shown as inaccessible, it is most likely a permissions problem."
        );
        tracing::warn!(
            "Your user needs read and write access to the probe's device node: a USB node under /dev/bus/usb, or a serial port such as /dev/ttyACM0."
        );
        tracing::warn!(
            "See {SETUP_URL} for how to set up the required udev rules and group membership."
        );
    }
}
