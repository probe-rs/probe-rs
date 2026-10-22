//! Compares the TC275 PFLASH with page aware sector backups (read only).
//!
//! A backup is a JSON file per logical sector: `{ base, size, blank: [0|1 per 32 B page], b64 }`,
//! where `blank` marks erased pages (they fail to read with an ECC error).
//!
//! ```text
//! cargo run -p probe-rs-infineon --example pflash_check -- <backup dir> [<sector>=<file.bin>...]
//! ```
//!
//! `<sector>=<file.bin>` additionally checks that the data of `file.bin` starts at `sector`
//! and that the rest of the 16 KiB sector is erased (or zero pages). A backup directory of `-`
//! skips the backup comparison.

use std::time::Duration;

use base64::Engine as _;
use probe_rs::{MemoryInterface, Permissions, probe::list::Lister};
use probe_rs_infineon::flash::{PAGE, read_pages};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = args
        .next()
        .unwrap_or_else(|| r"C:\work\aurix_flash_backup\2026-10-20_pflash".to_string());
    let extra = args.collect::<Vec<_>>();

    probe_rs_infineon::register_plugin();
    let probe = Lister::new().open("058b:0000".parse::<probe_rs::probe::DebugProbeSelector>()?)?;
    let mut session = probe.attach("TC275", Permissions::default())?;
    let mut core = session.core(0)?;
    let was_running = !core.core_halted()?;
    let mut bad = 0;

    // `-` skips the backup comparison.
    let entries = if dir == "-" {
        Vec::new()
    } else {
        std::fs::read_dir(&dir)?.collect::<Vec<_>>()
    };
    let mut files = entries
        .into_iter()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect::<Vec<_>>();
    files.sort();
    for file in files {
        let backup: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file)?)?;
        let base = backup["base"].as_u64().ok_or("no base")? as u32;
        let size = backup["size"].as_u64().ok_or("no size")? as usize;
        let blank = backup["blank"].as_array().ok_or("no blank")?;
        let image = base64::engine::general_purpose::STANDARD
            .decode(backup["b64"].as_str().ok_or("no b64")?)?;

        let pages = read_pages(&mut core, base, size)?;
        let mut differences = 0;
        for (index, page) in pages.iter().enumerate() {
            let erased = blank[index].as_u64() == Some(1);
            let expected = &image[index * PAGE..(index + 1) * PAGE];
            let same = match page {
                None => erased,
                Some(data) => !erased && data.as_slice() == expected,
            };
            if !same {
                differences += 1;
            }
        }
        let programmed = pages.iter().filter(|p| p.is_some()).count();
        println!(
            "[{}] {base:#010x} ({} KiB): {programmed} data page(s), {differences} difference(s)",
            if differences == 0 { "SAME" } else { "DIFF" },
            size / 1024
        );
        bad += (differences > 0) as usize;
    }

    for spec in extra {
        let (sector, path) = spec.split_once('=').ok_or("expected <sector>=<file.bin>")?;
        let base = u32::from_str_radix(sector.trim_start_matches("0x"), 16)?;
        let data = std::fs::read(path)?;
        let pages = read_pages(&mut core, base, 0x4000)?;
        let mut ok = true;
        for (index, page) in pages.iter().enumerate() {
            let start = index * PAGE;
            if start < data.len() {
                let mut expected = [0u8; PAGE];
                let n = (data.len() - start).min(PAGE);
                expected[..n].copy_from_slice(&data[start..start + n]);
                ok &= *page == Some(expected);
            } else {
                // Some flashers pad the last write with zero pages.
                ok &= page.is_none_or(|p| p == [0; PAGE]);
            }
        }
        let data_pages = pages.iter().filter(|p| p.is_some()).count();
        let mut head = vec![0u8; data.len()];
        let head_ok = core.read(base as u64, &mut head).is_ok() && head == data;
        println!(
            "[{}] {base:#010x}: {path} ({} B), rest erased; {data_pages} data page(s), data {}",
            if ok { "SAME" } else { "DIFF" },
            data.len(),
            if head_ok { "matches" } else { "differs" }
        );
        if !ok {
            for (index, page) in pages.iter().enumerate().filter(|(_, p)| p.is_some()).take(12) {
                let page = page.unwrap_or_default();
                println!("    page {index:3} @{:#010x}: {:02x?}", base + (index * PAGE) as u32, &page[..16]);
            }
        }
        bad += (!ok) as usize;
    }

    if was_running {
        core.run()?;
    }
    drop(core);
    std::thread::sleep(Duration::from_millis(10));
    println!("{bad} sector(s) differ");
    if bad > 0 {
        std::process::exit(1);
    }
    Ok(())
}
