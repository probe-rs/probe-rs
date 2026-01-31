//! Plugin system for probe-rs.
//!
//! This module contains the interfaces necessary to define and register plugins.
//! Plugins can extend the functionality of probe-rs by adding e.g. new targets, probes, image formats.
//!
//! Plugins are registered by calling the [`register_plugin`] function.

use crate::vendor::Vendor;

/// A plugin that can extend the functionality of probe-rs.
#[derive(Clone, Default)]
pub struct Plugin<'p> {
    /// A list of vendors to register with probe-rs.
    pub vendors: &'p [&'static dyn Vendor],
    // TODO: targets, image formats, probe drivers
}

/// Register a plugin.
pub fn register_plugin(plugin: Plugin<'_>) {
    // Implementation of plugin registration
    for vendor in plugin.vendors {
        crate::vendor::register_vendor(*vendor);
    }
}
