//! The target state that the compare backends share.

use std::time::Duration;

use crate::flashing::{ActiveFlasher, FlashAlgorithm, FlashError, FlashProgress, Flasher, Verify};
use crate::{Core, Session};

/// Only the chain of a [`Flasher`] holds backends that use the flash algorithm.
const NO_FLASHER: &str = "a compare without a flasher has no algorithm backends";

/// Gives the compare backends access to the target.
///
/// The context calls `Init` when a backend uses the flash algorithm and it is not initialized.
/// It calls `UnInit` before it gives the session or the core to a backend, and at the end. A
/// backend that uses the algorithm and the core in turn causes one `Init` and `UnInit` each
/// time.
pub struct CompareContext<'a> {
    session: &'a mut Session,
    core_index: usize,
    flasher: Option<&'a mut Flasher>,
    progress: FlashProgress<'static>,
    initialized: bool,
    init_failed: bool,
    faulted: bool,
}

impl<'a> CompareContext<'a> {
    pub(crate) fn new(session: &'a mut Session, flasher: &'a mut Flasher) -> Self {
        Self {
            session,
            core_index: flasher.core_index,
            flasher: Some(flasher),
            progress: FlashProgress::empty(),
            initialized: false,
            init_failed: false,
            faulted: false,
        }
    }

    /// Creates a context for a chain that has no backends that use the flash algorithm.
    pub(crate) fn without_algorithm(session: &'a mut Session, core_index: usize) -> Self {
        Self {
            session,
            core_index,
            flasher: None,
            progress: FlashProgress::empty(),
            initialized: false,
            init_failed: false,
            faulted: false,
        }
    }

    /// Returns the session.
    ///
    /// If an earlier backend initialized the flash algorithm, this calls `UnInit` first, and
    /// returns its error.
    pub fn session(&mut self) -> Result<&mut Session, FlashError> {
        self.uninit()?;
        self.invalidate_code();

        Ok(self.session)
    }

    /// Returns the core that the flash algorithm runs on.
    ///
    /// If an earlier backend initialized the flash algorithm, this calls `UnInit` first, and
    /// returns its error.
    pub fn core(&mut self) -> Result<Core<'_>, FlashError> {
        let core_index = self.core_index;
        self.session()?.core(core_index).map_err(FlashError::Core)
    }

    /// Returns the flash algorithm of this compare, without loading it.
    pub(crate) fn flash_algorithm(&self) -> Option<&FlashAlgorithm> {
        self.flasher.as_deref().map(Flasher::flash_algorithm)
    }

    /// Returns the flash algorithm, loaded and initialized for [`Verify`].
    pub(crate) fn algorithm(&mut self) -> Result<ActiveFlasher<'_, 'static, Verify>, FlashError> {
        let flasher = self.flasher.as_deref_mut().expect(NO_FLASHER);
        let mut active = flasher.activate(self.session, &mut self.progress)?;
        if !self.initialized {
            if let Err(error) = active.init(None) {
                self.init_failed = true;
                return Err(error);
            }
            self.initialized = true;
        }

        Ok(active)
    }

    /// Returns `true` if the flash algorithm cannot run after a backend error.
    pub(crate) fn faulted(&self) -> bool {
        self.faulted
    }

    /// Stops using the flash algorithm if a backend error happened in `Init` or while it was
    /// initialized.
    pub(crate) fn handle_fault(&mut self) {
        // A timeout in `Init` leaves the core running.
        if !self.initialized && !self.init_failed {
            return;
        }
        self.faulted = true;

        // The core can be in the fault handler of the application.
        let halted = self
            .session
            .core(self.core_index)
            .and_then(|mut core| core.halt(Duration::from_millis(500)));
        if let Err(error) = halted {
            tracing::debug!("Halt after a compare fault failed: {error}");
        }

        if let Err(error) = self.uninit() {
            tracing::debug!("Uninit after a compare fault failed: {error}");
        }
        self.invalidate_code();
    }

    /// Makes the next use of the flash algorithm write its code again.
    ///
    /// A fault or a backend with session access can overwrite the code in target RAM.
    fn invalidate_code(&mut self) {
        if let Some(flasher) = self.flasher.as_deref_mut() {
            flasher.code_valid = false;
        }
    }

    /// Calls `UnInit` if the flash algorithm is initialized.
    pub(crate) fn finish(mut self) -> Result<(), FlashError> {
        self.uninit()
    }

    fn uninit(&mut self) -> Result<(), FlashError> {
        if !self.initialized {
            return Ok(());
        }
        self.initialized = false;

        let flasher = self.flasher.as_deref_mut().expect(NO_FLASHER);
        let mut active = flasher.activate::<Verify>(self.session, &mut self.progress)?;
        active.uninit()
    }
}
