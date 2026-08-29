use gdbstub::{
    arch::Arch,
    target::{TargetError, ext::flash::Flash},
};
use probe_rs_rpc::FlashLoader;
use probe_rs_rpc::Key;
use probe_rs_rpc::flash::DownloadOptions;

use super::RuntimeTarget;

/// Upper bound for a single `flash/load_region` RPC payload.
const LOAD_REGION_CHUNK: usize = 64 * 1024;

// The GDB "load" command works as follow:
// - flash_erase is called first to erase all involved sectors. GDB uses the blocksize
//   defined in the memory map to provide sector-aligned addresses and lengths.
// - One flash_write command is issued for each object file section (e.g., .vector_table, .text, etc.)
//   that needs to be written to flash.
// - Finally, flash_done is called to indicate that flash programming operation is complete.
//
// Erase runs immediately over RPC. Writes are staged into a flash loader and committed in
// flash_done with skip_erase, since the sectors were already erased.
impl Flash for RuntimeTarget {
    fn flash_erase(
        &mut self,
        start_addr: <Self::Arch as Arch>::Usize,
        length: <Self::Arch as Arch>::Usize,
    ) -> gdbstub::target::TargetResult<(), Self> {
        // Drop any prior loader so a failed earlier load cannot leak staged data.
        let _drop = self.flash_loader.take();

        self.block_on(
            self.session
                .erase_range(start_addr, length, false, false, async |_| {}),
        )
        .map_err(|e| {
            tracing::error!(
                "GDB flash_erase failed for {length} bytes at {start_addr:#010x}: {e:#}"
            );
            TargetError::NonFatal
        })?;

        self.flash_erased = true;
        Ok(())
    }

    fn flash_write(
        &mut self,
        start_addr: <Self::Arch as Arch>::Usize,
        data: &[u8],
    ) -> gdbstub::target::TargetResult<(), Self> {
        let loader = self.ensure_flash_loader()?;

        let mut offset = 0usize;
        while offset < data.len() {
            let end = (offset + LOAD_REGION_CHUNK).min(data.len());
            let chunk = data[offset..end].to_vec();
            let address = start_addr + offset as u64;

            self.block_on(self.session.load_region(loader, address, chunk))
                .map_err(|e| {
                    tracing::error!(
                        "GDB flash_write failed to stage {} bytes at {:#010x}: {e:#}",
                        end - offset,
                        address
                    );
                    TargetError::NonFatal
                })?;

            offset = end;
        }

        Ok(())
    }

    fn flash_done(&mut self) -> gdbstub::target::TargetResult<(), Self> {
        let Some(loader) = self.flash_loader.take() else {
            self.flash_erased = false;
            return Err(TargetError::NonFatal);
        };

        let skip_erase = self.flash_erased;
        self.flash_erased = false;

        let options = DownloadOptions {
            skip_erase,
            ..DownloadOptions::default()
        };

        self.block_on(self.session.flash(options, loader, async |_| {}))
            .map_err(|e| {
                tracing::error!("GDB flash_done failed to commit flash programming: {e:#}");
                TargetError::NonFatal
            })?;

        Ok(())
    }
}

impl RuntimeTarget {
    fn ensure_flash_loader(&mut self) -> Result<Key<FlashLoader>, TargetError<anyhow::Error>> {
        if let Some(loader) = self.flash_loader {
            return Ok(loader);
        }

        let loader = self
            .block_on(self.session.new_flash_loader(false))
            .map_err(|e| {
                tracing::error!("GDB failed to create a flash loader: {e:#}");
                TargetError::NonFatal
            })?;
        self.flash_loader = Some(loader);
        Ok(loader)
    }
}
