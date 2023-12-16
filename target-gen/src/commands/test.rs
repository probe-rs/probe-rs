use std::cell::RefCell;
use std::fs::File;
use std::path::Path;
use std::rc::Rc;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use colored::Colorize;
use probe_rs::flashing::erase_all;
use probe_rs::MemoryInterface;
use probe_rs::{
    flashing::{erase_sectors, DownloadOptions, FlashLoader, FlashProgress},
    Permissions, Session,
};
use probe_rs_target::RawFlashAlgorithm;
use xshell::{cmd, Shell};

use crate::commands::elf::cmd_elf;

const ALGORITHM_NAME: &str = "algorithm-test";

pub fn cmd_test(
    target_artifact: &Path,
    template_path: &Path,
    definition_export_path: &Path,
    test_start_sector_address: Option<u64>,
) -> Result<()> {
    ensure_is_file(target_artifact)?;
    ensure_is_file(template_path)?;

    anyhow::ensure!(
        !definition_export_path.is_dir(),
        "'{}' is a directory. Please specify a file name.",
        definition_export_path.display()
    );

    // Generate the binary
    println!("Generating the YAML file in `{definition_export_path:?}`");
    std::fs::copy(template_path, definition_export_path).with_context(|| {
        format!(
            "Failed to copy template file from '{}' to '{}'",
            template_path.display(),
            definition_export_path.display()
        )
    })?;

    cmd_elf(
        target_artifact,
        true,
        Some(definition_export_path),
        true,
        Some(String::from(ALGORITHM_NAME)),
    )?;

    if let Err(error) = generate_debug_info(target_artifact) {
        println!("Generating debug artifacts failed because:");
        println!("{error}");
    }

    let file = File::open(Path::new(definition_export_path))?;
    probe_rs::config::add_target_from_yaml(file)?;
    let mut session =
        probe_rs::Session::auto_attach(ALGORITHM_NAME, Permissions::new().allow_erase_all())?;

    // Register callback to update the progress.
    let t = Rc::new(RefCell::new(Instant::now()));
    let progress = FlashProgress::new(move |event| {
        use probe_rs::flashing::ProgressEvent;
        match event {
            ProgressEvent::StartedProgramming { .. } => {
                let mut t = t.borrow_mut();
                *t = Instant::now();
            }
            ProgressEvent::StartedErasing => {
                let mut t = t.borrow_mut();
                *t = Instant::now();
            }
            ProgressEvent::FailedErasing => {
                println!("Failed erasing in {:?}", t.borrow().elapsed());
            }
            ProgressEvent::FinishedErasing => {
                println!("Finished erasing in {:?}", t.borrow().elapsed());
            }
            ProgressEvent::FailedProgramming => {
                println!("Failed programming in {:?}", t.borrow().elapsed());
            }
            ProgressEvent::FinishedProgramming => {
                println!("Finished programming in {:?}", t.borrow().elapsed());
            }
            ProgressEvent::DiagnosticMessage { message } => {
                let prefix = "Message".yellow();
                if message.ends_with('\n') {
                    print!("{prefix}: {message}");
                } else {
                    println!("{prefix}: {message}");
                }
            }
            _ => (),
        }
    });

    let flash_algorithm = if let Some(test_start_sector_address) = test_start_sector_address {
        let predicate = |x: &&RawFlashAlgorithm| {
            x.flash_properties.address_range.start <= test_start_sector_address
                && test_start_sector_address < x.flash_properties.address_range.end
        };
        let error_message = anyhow!("No flash algorithm matching specified address can be found");
        session
            .target()
            .flash_algorithms
            .iter()
            .find(predicate)
            .ok_or(error_message)?
    } else {
        &session.target().flash_algorithms[0]
    };
    let flash_properties = flash_algorithm.flash_properties.clone();
    let start_address = flash_properties.address_range.start;
    let end_address = flash_properties.address_range.end;
    let data_size = flash_properties.page_size;
    let erased_state = flash_properties.erased_byte_value;
    let sector_size = flash_properties.sectors[0].size;

    let test_start_sector_address = test_start_sector_address.unwrap_or(start_address);
    if test_start_sector_address < start_address
        || test_start_sector_address > start_address + end_address - sector_size * 2
        || test_start_sector_address % sector_size != 0
    {
        return Err(anyhow!(
            "test_start_sector_address must be sector aligned address pointing flash range"
        ));
    }
    let test_start_sector_index =
        ((test_start_sector_address - start_address) / sector_size) as usize;

    let test = "Test".green();
    println!("{test}: Erasing sectorwise and writing two pages ...");
    run_flash_erase(
        &mut session,
        progress.clone(),
        EraseSectors(test_start_sector_index, 2),
    )?;
    let mut readback = vec![0; (sector_size * 2) as usize];
    session
        .core(0)?
        .read_8(test_start_sector_address, &mut readback)?;
    assert!(
        !readback.iter().any(|v| *v != erased_state),
        "Not all sectors were erased"
    );

    let mut loader = session.target().flash_loader();
    let data = (0..data_size).map(|n| (n % 256) as u8).collect::<Vec<_>>();
    loader.add_data(test_start_sector_address + 1, &data)?;
    run_flash_download(&mut session, loader, progress.clone(), true)?;
    let mut readback = vec![0; data_size as usize];
    session
        .core(0)?
        .read_8(test_start_sector_address + 1, &mut readback)?;
    assert_eq!(readback, data);

    println!("{test}: Erasing the entire chip and writing two pages ...");
    run_flash_erase(&mut session, progress.clone(), EraseAll)?;
    let mut readback = vec![0; (sector_size * 2) as usize];
    session
        .core(0)?
        .read_8(test_start_sector_address, &mut readback)?;
    assert!(
        !readback.iter().any(|v| *v != erased_state),
        "Not all sectors were erased"
    );

    let mut loader = session.target().flash_loader();
    let data = (0..data_size).map(|n| (n % 256) as u8).collect::<Vec<_>>();
    loader.add_data(test_start_sector_address + 1, &data)?;
    run_flash_download(&mut session, loader, progress.clone(), true)?;
    let mut readback = vec![0; data_size as usize];
    session
        .core(0)?
        .read_8(test_start_sector_address + 1, &mut readback)?;
    assert_eq!(readback, data);

    println!("{test}: Erasing sectorwise and writing two pages double buffered ...");
    run_flash_erase(
        &mut session,
        progress.clone(),
        EraseSectors(test_start_sector_index, 2),
    )?;
    let mut readback = vec![0; (sector_size * 2) as usize];
    session
        .core(0)?
        .read_8(test_start_sector_address, &mut readback)?;
    assert!(
        !readback.iter().any(|v| *v != erased_state),
        "Not all sectors were erased"
    );

    let mut loader = session.target().flash_loader();
    let data = (0..data_size).map(|n| (n % 256) as u8).collect::<Vec<_>>();
    loader.add_data(test_start_sector_address + 1, &data)?;
    run_flash_download(&mut session, loader, progress, false)?;
    let mut readback = vec![0; data_size as usize];
    session
        .core(0)?
        .read_8(test_start_sector_address + 1, &mut readback)?;
    assert_eq!(readback, data);

    Ok(())
}

fn ensure_is_file(file_path: &Path) -> Result<()> {
    anyhow::ensure!(
        file_path.is_file(),
        "'{}' does not seem to be a valid file.",
        file_path.display()
    );

    Ok(())
}

/// Performs the flash download with the given loader. Ensure that the loader has the data to load already stored.
/// This function also manages the update and display of progress bars.
pub fn run_flash_download(
    session: &mut Session,
    loader: FlashLoader,
    progress: FlashProgress,
    disable_double_buffering: bool,
) -> Result<()> {
    let mut download_option = DownloadOptions::default();
    download_option.keep_unwritten_bytes = false;
    download_option.disable_double_buffering = disable_double_buffering;

    download_option.progress = Some(progress);
    download_option.skip_erase = true;

    loader.commit(session, download_option)?;

    Ok(())
}

pub enum EraseType {
    EraseAll,
    EraseSectors(usize, usize),
}
use EraseType::*;

/// Erases the entire flash or just the sectors specified.
pub fn run_flash_erase(
    session: &mut Session,
    progress: FlashProgress,
    erase_type: EraseType,
) -> Result<()> {
    if let EraseSectors(start_sector, sectors) = erase_type {
        erase_sectors(session, Some(progress), start_sector, sectors)?;
    } else {
        erase_all(session, Some(progress))?;
    }

    Ok(())
}

fn generate_debug_info(target_artifact: &Path) -> Result<()> {
    let sh = Shell::new()?;
    std::fs::write(
        "target/disassembly.s",
        cmd!(sh, "rust-objdump --disassemble {target_artifact}")
            .output()?
            .stdout,
    )?;
    std::fs::write(
        "target/dump.txt",
        cmd!(sh, "rust-objdump -x {target_artifact}")
            .output()?
            .stdout,
    )?;
    std::fs::write(
        "target/nm.txt",
        cmd!(sh, "rust-nm {target_artifact} -n").output()?.stdout,
    )?;

    Ok(())
}
