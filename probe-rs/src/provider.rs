use crate::architecture::arm::ArmChipInfo;
use crate::config::DebugSequence;
use crate::{Memory, Target};
use probe_rs_target::{Core, TargetDescriptionSource};

/// A target `Provider` is a generic container that can produce target `Family`s.
pub(crate) trait Provider: Send + Sync {
    /// Return the name of this provider.
    fn name(&self) -> &str;

    /// Return an iterator over the `Family` objects inside this `Provider`.
    ///
    /// The `Family` objects and the iterator itself inherit the lifetime of this `&self`
    /// borrow.
    fn families(&self) -> Box<dyn Iterator<Item = Box<dyn Family<'_> + '_>> + '_>;

    /// Attempt to autodetect an Arm chip, returning the detected `Variant`, if any.
    ///
    /// The `Variant` inherits the lifetime of this `&self` borrow.
    fn autodetect_arm<'a>(
        &'a self,
        arm_chip_info: &ArmChipInfo,
        memory: &mut Memory,
    ) -> Option<Box<dyn Variant<'a> + 'a>> {
        // Implementations should use `arm_chip_info` and possibly consult `memory`
        let _ = arm_chip_info;
        let _ = memory;
        None
    }
}

/// A target `Family` is a group of related `Variant`s.
///
/// The lifetime `'a` in `Family<'a>` is a borrow from `Provider`.
pub(crate) trait Family<'a> {
    /// Return the name of the `Family`.
    fn name(&self) -> &'a str;

    /// Return an iterator over the `Variant`s inside this `Family`.
    ///
    /// The `Variant`s and the iterator itself inherit the lifetime of the original `Provider`
    /// borrow.
    fn variants(&self) -> Box<dyn Iterator<Item = Box<dyn Variant<'a> + 'a>> + 'a>;
}

/// A `Variant` is a kind of device which can be instantiated into a `Target`.
///
/// The lifetime `'a` in `Variant<'a>` is a borrow from `Provider`.
pub(crate) trait Variant<'a> {
    /// Return the name of the `Variant`.
    fn name(&self) -> &'a str;

    /// Construct a `Target` for this `Variant`.
    fn to_target(&self) -> Target;
}

// Implement these traits for `probe-rs-target` types
mod probe_rs_target_glue;

#[cfg(feature = "builtin-targets")]
mod builtin;
pub(crate) use builtin::Builtin;

mod generic;
pub(crate) use generic::Generic;

mod file;
pub(crate) use file::File;
