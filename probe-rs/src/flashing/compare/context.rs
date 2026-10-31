//! The target state that the compare backends share.

use crate::flashing::{ActiveFlasher, FlashError, FlashProgress, Flasher, Verify};
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
            faulted: false,
        }
    }

    /// Returns the session.
    ///
    /// If an earlier backend initialized the flash algorithm, this calls `UnInit` first, and
    /// returns its error.
    pub fn session(&mut self) -> Result<&mut Session, FlashError> {
        self.uninit()?;

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

    /// Returns the flash algorithm, loaded and initialized for [`Verify`].
    pub(crate) fn algorithm(&mut self) -> Result<ActiveFlasher<'_, 'static, Verify>, FlashError> {
        let flasher = self.flasher.as_deref_mut().expect(NO_FLASHER);
        let mut active = flasher.activate(self.session, &mut self.progress)?;
        if !self.initialized {
            active.init(None)?;
            self.initialized = true;
        }

        Ok(active)
    }

    /// Returns `true` if the flash algorithm cannot run after a backend error.
    pub(crate) fn faulted(&self) -> bool {
        self.faulted
    }

    /// Stops using the flash algorithm if a backend error happened while it was initialized.
    pub(crate) fn handle_fault(&mut self) {
        if !self.initialized {
            return;
        }
        self.faulted = true;

        if let Err(error) = self.uninit() {
            tracing::debug!("Uninit after a compare fault failed: {error}");
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
