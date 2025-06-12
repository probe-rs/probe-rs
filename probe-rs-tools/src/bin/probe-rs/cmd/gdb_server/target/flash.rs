use gdbstub::{
    arch::Arch,
    target::{TargetError, ext::flash::Flash},
};
use probe_rs::flashing::DownloadOptions;

use super::RuntimeTarget;

// The GDB "load" command works as follow:
// - flash_erase is called first to erase all involved sectors. GDB uses the blocksize
//   defined in the memory map to provide sector-aligned addresses and lengths.
// - One flash_write command is issued for each object file section (e.g., .vector_table, .text, etc.)
//   that needs to be written to flash.
// - Finally, flash_done is called to indicate that flash programming operation is complete.
//   According to the GDB documentation, we are allowed to delay and batch all the erase/write
//   operations until flash_done is invoked.

// In our implementation, we collect all the write operations in the FlashLoader
// and ignore the flash_erase command, as the FlashLoader will handle everything
// when we commit during the flash_done command.
impl Flash for RuntimeTarget<'_> {
    fn flash_erase(
        &mut self,
        _start_addr: <Self::Arch as Arch>::Usize,
        _length: <Self::Arch as Arch>::Usize,
    ) -> gdbstub::target::TargetResult<(), Self> {
        // We drop the flash_loader to ensure a fresh start in case
        // flash_write returns an error and flash_done is not called.
        let _drop = self.flash_loader.take();
        Ok(())
    }

    fn flash_write(
        &mut self,
        start_addr: <Self::Arch as Arch>::Usize,
        data: &[u8],
    ) -> gdbstub::target::TargetResult<(), Self> {
        let flash_loader = self
            .flash_loader
            .get_or_insert_with(|| self.session.lock().target().flash_loader());

        flash_loader
            .add_data(start_addr, data)
            .map_err(|_e| TargetError::NonFatal)?;
        Ok(())
    }

    fn flash_done(&mut self) -> gdbstub::target::TargetResult<(), Self> {
        let flash_loader = self.flash_loader.as_mut().ok_or(TargetError::NonFatal)?;
        let mut session = self.session.lock();
        flash_loader
            .commit(&mut session, DownloadOptions::default())
            .map_err(|_e| TargetError::NonFatal)?;

        let _drop = self.flash_loader.take();
        Ok(())
    }
}
