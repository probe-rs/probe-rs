//! Sequences for the RP2040.

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use crate::architecture::arm::{
    communication_interface::DapProbe, sequences::ArmDebugSequence, ArmError, DpAddress,
};

/// The debug sequence implementation for the RP2040.
#[derive(Debug)]
pub struct Rp2040 {}

impl Rp2040 {
    /// Creates a new debug sequence handle for the RP2040.
    pub fn create() -> Arc<Rp2040> {
        Arc::new(Rp2040 {})
    }
}

// Note: If you add a sequence implementation, you should also add a delegate to the rescue implementation.
impl ArmDebugSequence for Rp2040 {}

/// The debug sequence implementation for the RP2040 to rescue bricked devices.
///
/// This is a thin wrapper around the normal RP2040 sequence that first connects to a special
/// DP address to force the chip into a safe state before connecting to the actual DP address.
#[derive(Debug)]
pub struct Rp2040Rescue {
    inner: Rp2040,
    connected: AtomicBool,
}

impl Rp2040Rescue {
    /// Creates a new debug sequence handle for the RP2040 to rescue bricked devices.
    pub fn create() -> Arc<Rp2040Rescue> {
        Arc::new(Rp2040Rescue {
            inner: Rp2040 {},
            connected: AtomicBool::new(false),
        })
    }
}

impl ArmDebugSequence for Rp2040Rescue {
    fn debug_port_setup(
        &self,
        interface: &mut dyn DapProbe,
        dp: DpAddress,
    ) -> Result<(), ArmError> {
        // Connect to, then power down the Rescue DP address to force the chip into a safe state.
        if !self.connected.load(Ordering::Relaxed) {
            self.inner
                .debug_port_setup(interface, DpAddress::Multidrop(0xf1002927))?;

            std::thread::sleep(Duration::from_millis(100));

            // The rescue DP should have done its job at this point, power it down.
            self.inner.debug_port_stop(interface, dp)?;

            self.connected.store(true, Ordering::Relaxed);
        }

        // Now we can connect to the actual DP address.
        self.inner.debug_port_setup(interface, dp)
    }
}
