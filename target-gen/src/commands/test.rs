use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::time::Instant;

use anyhow::Result;
use colored::Colorize;
use probe_rs::flashing::erase_all;
use probe_rs::MemoryInterface;
use probe_rs::{
    flashing::{erase_sectors, DownloadOptions, FlashLoader, FlashProgress},
    Permissions, Session,
};
use probe_rs_cli_util::logging::println;
use xshell::{cmd, Shell};

use crate::commands::elf::cmd_elf;

const ALGORITHM_NAME: &str = "algorithm-test";

pub fn cmd_test(
    target_artifact: &Path,
    template_path: &Path,
    definition_export_path: &Path,
) -> Result<()> {
    // Generate the binary
    println("Generating the YAML file in `{definition_export_path}`");
    std::fs::copy(template_path, definition_export_path)?;
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

    probe_rs::config::add_target_from_yaml(definition_export_path)?;
    let mut session =
        probe_rs::Session::auto_attach(ALGORITHM_NAME, Permissions::new().allow_erase_all())?;

    let data_size = probe_rs::config::get_target_by_name(ALGORITHM_NAME)?.flash_algorithms[0]
        .flash_properties
        .page_size;

    // Register callback to update the progress.
    let t = Rc::new(RefCell::new(Instant::now()));
    let progress = FlashProgress::new(move |event| {
        use probe_rs::flashing::ProgressEvent::*;
        match event {
            StartedProgramming => {
                let mut t = t.borrow_mut();
                *t = Instant::now();
            }
            StartedErasing => {
                let mut t = t.borrow_mut();
                *t = Instant::now();
            }
            FailedErasing => {
                println!("Failed erasing in {:?}", t.borrow().elapsed());
            }
            FinishedErasing => {
                println!("Finished erasing in {:?}", t.borrow().elapsed());
            }
            FailedProgramming => {
                println!("Failed programming in {:?}", t.borrow().elapsed());
            }
            FinishedProgramming => {
                println!("Finished programming in {:?}", t.borrow().elapsed());
            }
            DiagnosticMessage { message } => {
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

    let test = "Test".green();
    let flash_properties = session.target().flash_algorithms[0]
        .flash_properties
        .clone();
    let erased_state = flash_properties.erased_byte_value;

    println!("{test}: Erasing sectorwise and writing two pages ...");

    run_flash_erase(&mut session, &progress, false)?;
    // TODO: The sector used here is not necessarily the sector the flash algorithm targets.
    // Make this configurable.
    let mut readback = vec![0; flash_properties.sectors[0].size as usize];
    session.core(0)?.read_8(0x0, &mut readback)?;
    assert!(
        !readback.iter().any(|v| *v != erased_state),
        "Not all sectors were erased"
    );

    let mut loader = session.target().flash_loader();
    let data = (0..data_size)
        .into_iter()
        .map(|n| (n % 256) as u8)
        .collect::<Vec<_>>();
    loader.add_data(0x1, &data)?;
    run_flash_download(&mut session, loader, &progress, true)?;
    let mut readback = vec![0; data_size as usize];
    session.core(0)?.read_8(0x1, &mut readback)?;
    assert_eq!(readback, data);

    println!("{test}: Erasing the entire chip and writing two pages ...");
    run_flash_erase(&mut session, &progress, true)?;
    // TODO: The sector used here is not necessarily the sector the flash algorithm targets.
    // Make this configurable.
    let mut readback = vec![0; flash_properties.sectors[0].size as usize];
    session.core(0)?.read_8(0x0, &mut readback)?;
    assert!(
        !readback.iter().any(|v| *v != erased_state),
        "Not all sectors were erased"
    );

    let mut loader = session.target().flash_loader();
    let data = (0..data_size)
        .into_iter()
        .map(|n| (n % 256) as u8)
        .collect::<Vec<_>>();
    loader.add_data(0x1, &data)?;
    run_flash_download(&mut session, loader, &progress, true)?;
    let mut readback = vec![0; data_size as usize];
    session.core(0)?.read_8(0x1, &mut readback)?;
    assert_eq!(readback, data);

    println!("{test}: Erasing sectorwise and writing two pages double buffered ...");
    run_flash_erase(&mut session, &progress, false)?;
    // TODO: The sector used here is not necessarily the sector the flash algorithm targets.
    // Make this configurable.
    let mut readback = vec![0; flash_properties.sectors[0].size as usize];
    session.core(0)?.read_8(0x0, &mut readback)?;
    assert!(
        !readback.iter().any(|v| *v != erased_state),
        "Not all sectors were erased"
    );

    let mut loader = session.target().flash_loader();
    let data = (0..data_size)
        .into_iter()
        .map(|n| (n % 256) as u8)
        .collect::<Vec<_>>();
    loader.add_data(0x1, &data)?;
    run_flash_download(&mut session, loader, &progress, false)?;
    let mut readback = vec![0; data_size as usize];
    session.core(0)?.read_8(0x1, &mut readback)?;
    assert_eq!(readback, data);

    Ok(())
}

/// Performs the flash download with the given loader. Ensure that the loader has the data to load already stored.
/// This function also manages the update and display of progress bars.
pub fn run_flash_download(
    session: &mut Session,
    loader: FlashLoader,
    progress: &FlashProgress,
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

/// Erases the entire flash if `do_chip_erase` is true,
/// Otherwise it erases sectors 0 and 1.
pub fn run_flash_erase(
    session: &mut Session,
    progress: &FlashProgress,
    do_chip_erase: bool,
) -> Result<()> {
    if do_chip_erase {
        erase_all(session, Some(progress))?;
    } else {
        erase_sectors(session, Some(progress), 0, 2)?;
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
