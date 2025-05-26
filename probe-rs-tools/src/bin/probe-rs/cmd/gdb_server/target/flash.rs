use gdbstub::{
    arch::Arch,
    target::{TargetError, ext::flash::Flash},
};
use probe_rs::flashing::DownloadOptions;

use super::RuntimeTarget;

// GDB "load" command works as follow:
// - flash_erase is called first to erase all involved sectors. GDB uses the blocksize
//   in the memory map to provide sector aligned address and length.
// - Issues one flash_write command for each object file section (e.g: .vector_table, .text ...)
//   that needs to be written into the flash.
// - flash_done is called last to indicate that flash programming operation is finished. By the GDB documentation
//   we are allowed to delay and batch all the erase/write operations until flash_done is called.
//
// Here we collect all the write operations in the flash loader and ignore flash_erase command because
// the FlashLoader will take care of everything when we commit in the flash_done command.
impl Flash for RuntimeTarget<'_> {
    fn flash_erase(
        &mut self,
        _start_addr: <Self::Arch as Arch>::Usize,
        _length: <Self::Arch as Arch>::Usize,
    ) -> gdbstub::target::TargetResult<(), Self> {
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
