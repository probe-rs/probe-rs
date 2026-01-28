//! Plugin system for probe-rs.
//!
//! This module contains the interfaces necessary to define and register plugins.
//! Plugins can extend the functionality of probe-rs by adding e.g. new targets, probes, image formats.
//!
//! Plugins are registered by calling the [`register_plugin`] function.

use probe_rs_target::ChipFamily;

use crate::{flashing::ImageFormat, vendor::Vendor};

/// A plugin that can extend the functionality of probe-rs.
#[derive(Clone, Default)]
pub struct Plugin<'p> {
    /// A list of vendors to register with probe-rs.
    pub vendors: &'p [&'static dyn Vendor],

    /// A list of image formats to register with probe-rs.
    pub image_formats: &'p [&'static dyn ImageFormat],

    /// A list of targets to register with probe-rs.
    pub targets: &'p [ChipFamily],
    // TODO: probe drivers
}

/// Register a plugin.
pub fn register_plugin(plugin: Plugin<'_>) {
    // Implementation of plugin registration
    for vendor in plugin.vendors {
        crate::vendor::register_vendor(*vendor);
    }
    for image_format in plugin.image_formats {
        crate::flashing::register_image_format(*image_format);
    }
    for target in plugin.targets {
        crate::config::registry::add_builtin_target(target.clone());
    }
}
