use std::path::Path;
use std::time::Instant;

use crate::commands::elf::cmd_elf;
use anyhow::{Context, Result, anyhow};
use colored::Colorize;
use probe_rs::flashing::Flasher;
use probe_rs::{
    MemoryInterface, Permissions, Session, SessionConfig,
    config::Registry,
    flashing::{
        CompareOutcome, DownloadOptions, FlashLoader, FlashProgress, Outside, ProgressEvent,
        ProgressOperation, SectorCheck, erase, erase_all, run_blank_check,
    },
    probe::{DebugProbeSelector, WireProtocol, list::Lister},
};
use probe_rs_target::RawFlashAlgorithm;
use xshell::{Shell, cmd};

#[expect(clippy::too_many_arguments)]
pub fn cmd_test(
    target_artifact: &Path,
    template_path: &Path,
    definition_export_path: &Path,
    test_start_sector_address: Option<u64>,
    chip: Option<String>,
    name: Option<String>,
    probe: Option<DebugProbeSelector>,
    speed: Option<u32>,
    protocol: Option<WireProtocol>,
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
        name,
    )?;

    if let Err(error) = generate_debug_info(target_artifact) {
        println!("Generating debug artifacts failed because:");
        println!("{error}");
    }

    let mut registry = Registry::new();

    // Add the target to the registry from the generated YAML file
    let yaml = std::fs::read_to_string(definition_export_path)?;
    let family_name = registry.add_target_family_from_yaml(&yaml)?;

    let targets = registry
        .get_targets_by_family_name(&family_name)
        .with_context(|| format!("Failed to get targets of {family_name}"))?;

    let target_name = match targets.len() {
        0 => return Err(anyhow!("No targets found for family {family_name}")),
        1 => &targets[0],
        count if chip.is_none() => {
            return Err(anyhow!(
                "{count} targets found for family {family_name}: {targets:#?}. Specify the desired target with --chip."
            ));
        }
        _ => {
            let chip = chip.as_ref().unwrap();
            targets
                .iter()
                .find(|target| *target == chip)
                .with_context(|| format!("No target found for chip {chip}"))?
        }
    };

    // Create SessionConfig to steer auto attach
    let permissions = Permissions::new().allow_erase_all();
    let session_config = SessionConfig {
        permissions,
        speed,
        protocol,
    };

    let lister = Lister::new();
    let available_probes = lister.list(probe.as_ref());
    if available_probes.len() > 1 {
        return Err(anyhow!(
            "Multiple probes were found -- please specify a probe with `--probe`"
        ));
    }
    let Some(probe) = available_probes.first() else {
        return Err(anyhow!("No probes were found"));
    };
    let mut probe = probe.open()?;

    // If the caller has specified speed or protocol in SessionConfig, set them
    if let Some(speed) = session_config.speed {
        probe.set_speed(speed)?;
    }

    if let Some(protocol) = session_config.protocol {
        probe.select_protocol(protocol)?;
    }

    // We need to get the chip name so that special startup procedure can be used. (matched on name)
    let mut session =
        probe.attach_with_registry(target_name, session_config.permissions, &registry)?;

    // Register callback to update the progress.
    let mut progress = progress_callbacks();

    let (algo_index, flash_algorithm) = if let Some(test_start_sector_address) =
        test_start_sector_address
    {
        let predicate = |(_, x): &(usize, &RawFlashAlgorithm)| {
            x.flash_properties.address_range.start <= test_start_sector_address
                && test_start_sector_address < x.flash_properties.address_range.end
        };
        let error_message = anyhow!("No flash algorithm matching specified address can be found");
        session
            .target()
            .flash_algorithms
            .iter()
            .enumerate()
            .find(predicate)
            .ok_or(error_message)?
    } else {
        (0, &session.target().flash_algorithms[0])
    };

    let have_read = flash_algorithm.pc_read.is_some();
    let have_verify = flash_algorithm.pc_verify.is_some();

    let flash_properties = &flash_algorithm.flash_properties;
    let start_address = flash_properties.address_range.start;
    let end_address = flash_properties.address_range.end;
    let data_size = flash_properties.page_size;
    let erased = flash_properties.erased_byte_value;
    let sector_size = flash_properties.sectors[0].size;

    let test_start_sector_address = test_start_sector_address.unwrap_or(start_address);
    if test_start_sector_address < start_address
        || test_start_sector_address > start_address + end_address - sector_size * 2
        || !test_start_sector_address.is_multiple_of(sector_size)
    {
        return Err(anyhow!(
            "test_start_sector_address must be sector aligned address pointing flash range"
        ));
    }

    let test = "Test".green();
    println!("{test}: Erasing sectorwise and writing two pages ...");
    run_flash_erase(
        &mut session,
        EraseType::EraseRange(
            test_start_sector_address,
            test_start_sector_address + sector_size * 2,
        ),
    )?;

    println!("{test}: Erase done");

    run_blank_check(
        &mut session,
        &mut progress,
        test_start_sector_address,
        test_start_sector_address + sector_size * 2,
        true,
    )?;

    let data = (0..data_size).map(|n| (n % 256) as u8).collect::<Vec<_>>();
    let sector = test_start_sector_address..test_start_sector_address + sector_size;
    // The written data can run into the next sector. Check the part in the first one.
    let in_sector = data.len().min(sector_size as usize - 1);
    let image = [(test_start_sector_address + 1, &data[..in_sector])];

    let signatures = run_signature_check(
        &mut session,
        algo_index,
        &sector,
        &image,
        erased,
        CompareOutcome::Empty,
    )?;
    if signatures {
        println!("{test}: Signature of an erased sector OK");
    }

    println!("{test}: Writing two pages ...");

    let mut loader = session.target().flash_loader();
    loader.read_rtt_output(true);
    loader.add_data(test_start_sector_address + 1, &data)?;
    run_flash_download(&mut session, &mut loader, true)?;

    println!("{test}: Write done");

    if signatures {
        run_signature_check(
            &mut session,
            algo_index,
            &sector,
            &image,
            erased,
            CompareOutcome::Match,
        )?;
        let mut changed = data[..in_sector].to_vec();
        changed[0] ^= 0xFF;
        run_signature_check(
            &mut session,
            algo_index,
            &sector,
            &[(test_start_sector_address + 1, changed.as_slice())],
            erased,
            CompareOutcome::Mismatch,
        )?;
        println!("{test}: Signatures of a written sector OK");
    }

    let mut readback = vec![0; data_size as usize];

    if have_read {
        println!("{test}: Reading back two pages (via API) ...");
        run_read_flash(
            &mut session,
            algo_index,
            test_start_sector_address + 1,
            &mut readback,
        )?;
    } else {
        println!("{test}: Reading back two pages (via core) ...");
        session
            .core(0)?
            .read(test_start_sector_address + 1, &mut readback)?;
    }
    assert_eq!(readback, data);
    println!("{test}: Write verified OK");

    println!("{test}: Erasing the entire chip and writing two pages ...");
    run_flash_erase(&mut session, EraseType::EraseAll)?;
    println!("{test}: Erase done");
    run_blank_check(
        &mut session,
        &mut progress,
        test_start_sector_address,
        test_start_sector_address + sector_size * 2,
        true,
    )?;

    println!("{test}: Writing two pages ...");
    let mut loader = session.target().flash_loader();
    loader.read_rtt_output(true);
    let data = (0..data_size).map(|n| (n % 256) as u8).collect::<Vec<_>>();
    loader.add_data(test_start_sector_address + 1, &data)?;
    run_flash_download(&mut session, &mut loader, true)?;

    println!("{test}: Write done");

    let mut readback = vec![0; data_size as usize];
    if have_read {
        println!("{test}: Reading back two pages (via API) ...");
        run_read_flash(
            &mut session,
            algo_index,
            test_start_sector_address + 1,
            &mut readback,
        )?;
    } else {
        println!("{test}: Reading back two pages (via core) ...");
        session
            .core(0)?
            .read_8(test_start_sector_address + 1, &mut readback)?;
    }
    assert_eq!(readback, data);
    println!("{test}: Write verified OK");

    println!("{test}: Erasing sectorwise and writing two pages double buffered ...");
    run_flash_erase(
        &mut session,
        EraseType::EraseRange(
            test_start_sector_address,
            test_start_sector_address + sector_size * 2,
        ),
    )?;
    println!("{test}: Erase done");

    run_blank_check(
        &mut session,
        &mut progress,
        test_start_sector_address,
        test_start_sector_address + sector_size * 2,
        true,
    )?;

    println!("{test}: Writing two pages ...");
    let mut loader = session.target().flash_loader();
    loader.read_rtt_output(true);
    let data = (0..data_size).map(|n| (n % 256) as u8).collect::<Vec<_>>();
    loader.add_data(test_start_sector_address + 1, &data)?;
    run_flash_download(&mut session, &mut loader, false)?;
    println!("{test}: Write done");

    if have_verify {
        println!("{test}: Verifying");
        let mut progress = progress_callbacks();
        loader.verify(&mut session, &mut progress)?;
        println!("{test}: verification done");
    }

    let mut readback = vec![0; data_size as usize];
    if have_read {
        println!("{test}: Reading back two pages (via API) ...");
        run_read_flash(
            &mut session,
            algo_index,
            test_start_sector_address + 1,
            &mut readback,
        )?;
    } else {
        println!("{test}: Reading back two pages (via core) ...");
        session
            .core(0)?
            .read_8(test_start_sector_address + 1, &mut readback)?;
    }
    assert_eq!(readback, data);
    println!("{test}: Write verified OK");

    Ok(())
}

fn progress_callbacks() -> FlashProgress<'static> {
    FlashProgress::new({
        let mut t = Instant::now();

        move |event| match event {
            ProgressEvent::Started(ProgressOperation::Program) => t = Instant::now(),
            ProgressEvent::Started(ProgressOperation::Erase) => t = Instant::now(),
            ProgressEvent::Failed(ProgressOperation::Erase) => {
                println!("Failed erasing in {:?}", t.elapsed());
            }
            ProgressEvent::Finished(ProgressOperation::Erase) => {
                println!("Finished erasing in {:?}", t.elapsed());
            }
            ProgressEvent::Failed(ProgressOperation::Program) => {
                println!("Failed programming in {:?}", t.elapsed());
            }
            ProgressEvent::Finished(ProgressOperation::Program) => {
                println!("Finished programming in {:?}", t.elapsed());
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
    })
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
    loader: &mut FlashLoader,
    disable_double_buffering: bool,
) -> Result<()> {
    let mut download_option = DownloadOptions::default();

    download_option.keep_unwritten_bytes = false;
    download_option.disable_double_buffering = disable_double_buffering;
    download_option.progress = progress_callbacks();
    download_option.skip_erase = true;

    loader.commit(session, download_option)?;

    Ok(())
}

pub enum EraseType {
    EraseAll,
    EraseRange(u64, u64),
}

/// Erases the entire flash or just the sectors specified.
pub fn run_flash_erase(session: &mut Session, erase_type: EraseType) -> Result<()> {
    let mut progress = progress_callbacks();
    if let EraseType::EraseRange(start, end) = erase_type {
        erase(session, &mut progress, start, end, false, true)?;
    } else {
        erase_all(session, &mut progress, true)?;
    }

    Ok(())
}

pub fn run_read_flash(
    session: &mut Session,
    algo_index: usize,
    address: u64,
    data: &mut [u8],
) -> Result<()> {
    let mut progress = progress_callbacks();

    let raw_flash_algorithm = &session.target().flash_algorithms[algo_index];
    let mut flasher = Flasher::new(session.target(), 0, raw_flash_algorithm)?.with_rtt();

    flasher.run_verify(session, &mut progress, |active, _region| {
        active.read_flash(address, data)
    })?;
    Ok(())
}

/// Compares one sector with the signature function of the flash algorithm.
///
/// Returns `false` if the algorithm has no signature function with a known hash. An erased
/// sector can also give a mismatch: a signature function does not have to detect erased
/// flash.
fn run_signature_check(
    session: &mut Session,
    algo_index: usize,
    sector: &std::ops::Range<u64>,
    image: &[(u64, &[u8])],
    erased: u8,
    expected: CompareOutcome,
) -> Result<bool> {
    let mut progress = progress_callbacks();

    let raw_flash_algorithm = &session.target().flash_algorithms[algo_index];
    let mut flasher = Flasher::new(session.target(), 0, raw_flash_algorithm)?.with_rtt();

    let check = SectorCheck {
        sector: sector.clone(),
        expected: image.to_vec(),
        outside: Outside::Erased(erased),
    };

    let started = Instant::now();
    let result = flasher.run_verify(session, &mut progress, |active, _region| {
        active.compare_with_signatures(std::slice::from_ref(&check))
    })?;

    let Some((hash, outcomes)) = result else {
        return Ok(false);
    };

    println!(
        "{}: Signature ({hash}) of {:#010x?}: {:?} in {:?}",
        "Test".green(),
        sector,
        outcomes[0],
        started.elapsed()
    );

    match (expected, outcomes[0]) {
        (expected, outcome) if expected == outcome => {}
        (CompareOutcome::Empty, CompareOutcome::Mismatch) => {
            println!(
                "{}: The signature function does not detect erased flash.",
                "Warning".yellow()
            );
        }
        (expected, outcome) => {
            return Err(anyhow!(
                "Signature check of {sector:#010x?} gave {outcome:?}, expected {expected:?}"
            ));
        }
    }

    Ok(true)
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
