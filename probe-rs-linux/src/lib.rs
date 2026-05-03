//! Linux-specific probe drivers for probe-rs.

#[cfg(target_os = "linux")]
mod linuxgpiod;

/// Register the Linux probe drivers with probe-rs.
///
/// On non-Linux targets this is a no-op so callers can register the plugin
/// unconditionally.
pub fn register_plugin() {
    #[cfg(target_os = "linux")]
    {
        use probe_rs::plugin::{Plugin, register_plugin};
        register_plugin(Plugin {
            vendors: &[],
            image_formats: &[],
            targets: &[],
            probe_drivers: &[&linuxgpiod::LinuxGpiodFactory],
        });
    }
}
