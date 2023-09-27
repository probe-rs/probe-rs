//! This module contains the implementation of the [`crate::core::ExceptionInterface`] for the various ARM core variants.
pub(crate) mod armv6m;
/// Where applicable, this defines shared logic for implementing exception handling accross the various ARMv6-m and ARMv7-m [`crate::CoreType`]'s.
pub(crate) mod armv6m_armv7m_shared;
// NOTE: There is also a [`CoreType::Armv7em`] variant, but it is not currently used/implemented in probe-rs.
pub(crate) mod armv7m;
pub(crate) mod armv7a {
    use crate::core::ExceptionInterface;
    impl<'probe> ExceptionInterface for crate::architecture::arm::core::armv7a::Armv7a<'probe> {}
}

pub(crate) mod armv8a {
    use crate::core::ExceptionInterface;
    impl<'probe> ExceptionInterface for crate::architecture::arm::core::armv8a::Armv8a<'probe> {}
}

pub(crate) mod armv8m;
