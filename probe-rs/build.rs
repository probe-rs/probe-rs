use std::env;
use std::fs::{read_dir, read_to_string, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

fn main() {
    let out_dir = env::var("OUT_DIR").unwrap();
    let dest_path = Path::new(&out_dir).join("targets.rs");
    let mut f = File::create(&dest_path).unwrap();
    let root_dir = Path::new(env!("CARGO_MANIFEST_DIR"));

    // Determine all config files to parse.
    let mut files = vec![];
    visit_dirs(Path::new("targets"), &mut files).unwrap();

    let mut configs: Vec<proc_macro2::TokenStream> = vec![];
    let mut config_names: Vec<String> = vec![];
    for file in files {
        let string = read_to_string(&file).expect(
            "Algorithm definition file could not be read. This is a bug. Please report it.",
        );

        let yaml: Result<serde_yaml::Value, _> = serde_yaml::from_str(&string);

        match yaml {
            Ok(chip) => {
                let (name, chip) = extract_chip(&chip);
                config_names.push(name);
                configs.push(chip);
            },
            Err(e) => {
                panic!("Failed to parse target file: {:?} because:\n{}", file, e);
            }
        }
    }

    let stream: String = format!(
        "{}",
        quote::quote! {
            lazy_static::lazy_static! {
                static ref TARGETS: Vec<(&'static str, Chip)> = vec![
                    #((#config_names, #configs),)*
                ];
            }
        }
    );

    f.write_all(stream.as_bytes())
        .expect("Writing build.rs output failed.");
}

// one possible implementation of walking a directory only visiting files
fn visit_dirs(dir: &Path, targets: &mut Vec<PathBuf>) -> io::Result<()> {
    if dir.is_dir() {
        for entry in read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                visit_dirs(&path, targets)?;
            } else {
                targets.push(path.to_owned());
            }
        }
    }
    Ok(())
}

/// Creates a properly quoted Option<T>` `TokenStream` from an `Option<T>`.
fn quote_option<T: quote::ToTokens>(option: Option<T>) -> proc_macro2::TokenStream {
    if let Some(value) = option {
        quote::quote! {
            Some(#value)
        }
    } else {
        quote::quote! {
            None
        }
    }
}

/// Extracts a list of algorithm token streams from a yaml value.
fn extract_algorithms(chip: &serde_yaml::Value) -> Vec<proc_macro2::TokenStream> {
    // Get an iterator over all the algorithms contained in the chip value obtained from the yaml file.
    let algorithm_iter = chip
        .get("flash_algorithms")
        .unwrap()
        .as_sequence()
        .unwrap()
        .iter();

    algorithm_iter.map(|algorithm| {
        // Extract all values and form them into a struct.
        let name = algorithm.get("name").unwrap().as_str().unwrap();
        let default = algorithm.get("default").unwrap().as_bool().unwrap();
        let load_address = algorithm.get("load_address").unwrap().as_u64().unwrap() as u32;
        let instructions = algorithm.get("instructions").unwrap().as_sequence().unwrap().iter().map(|v| v.as_u64().unwrap() as u32);
        let pc_init = quote_option(algorithm.get("pc_init").unwrap().as_u64().map(|v| v as u32));
        let pc_uninit = quote_option(algorithm.get("pc_uninit").unwrap().as_u64().map(|v| v as u32));
        let pc_program_page = algorithm.get("pc_program_page").unwrap().as_u64().unwrap() as u32;
        let pc_erase_sector = algorithm.get("pc_erase_sector").unwrap().as_u64().unwrap() as u32;
        let pc_erase_all = quote_option(algorithm.get("pc_erase_all").unwrap().as_u64().map(|v| v as u32));
        let static_base = algorithm.get("static_base").unwrap().as_u64().unwrap() as u32;
        let begin_stack = algorithm.get("begin_stack").unwrap().as_u64().unwrap() as u32;
        let begin_data = algorithm.get("begin_data").unwrap().as_u64().unwrap() as u32;
        let page_buffers = algorithm.get("page_buffers").unwrap().as_sequence().unwrap().iter().map(|v| v.as_u64().unwrap() as u32);
        let min_program_length = quote_option(algorithm.get("min_program_length").unwrap().as_u64().map(|v| v as u32));
        let analyzer_supported = algorithm.get("analyzer_supported").unwrap().as_bool().unwrap();
        let analyzer_address = algorithm.get("analyzer_address").unwrap().as_u64().unwrap() as u32;

        // Quote the algorithm struct.
        let algorithm = quote::quote!{
            FlashAlgorithm {
                name: #name.to_owned(),
                default: #default,
                load_address: #load_address,
                instructions: vec![
                    #(#instructions,)*
                ],
                pc_init: #pc_init,
                pc_uninit: #pc_uninit,
                pc_program_page: #pc_program_page,
                pc_erase_sector: #pc_erase_sector,
                pc_erase_all: #pc_erase_all,
                static_base: #static_base,
                begin_stack: #begin_stack,
                begin_data: #begin_data,
                page_buffers: vec![
                    #(#page_buffers,)*
                ],
                min_program_length: #min_program_length,
                analyzer_supported: #analyzer_supported,
                analyzer_address: #analyzer_address,
            }
        };

        algorithm
    }).collect()
}

/// Extracts a list of algorithm token streams from a yaml value.
fn extract_memory_map(chip: &serde_yaml::Value) -> Vec<proc_macro2::TokenStream> {
    // Get an iterator over all the algorithms contained in the chip value obtained from the yaml file.
    let memory_map_iter = chip
        .get("memory_map")
        .unwrap()
        .as_sequence()
        .unwrap()
        .iter();

    memory_map_iter.filter_map(|memory_region| {
        // Check if it's a RAM region. If yes, parse it into a TokenStream.
        memory_region.get("Ram").map(|region| {
            let range = region.get("range").unwrap();
            let start = range.get("start").unwrap().as_u64().unwrap() as u32;
            let end = range.get("end").unwrap().as_u64().unwrap() as u32;
            let is_boot_memory = region.get("is_boot_memory").unwrap().as_bool().unwrap();

            quote::quote!{
                MemoryRegion::Ram(RamRegion {
                    range: #start..#end,
                    is_boot_memory: #is_boot_memory,
                })
            }
        })
        .or_else(|| {
            memory_region.get("Flash").map(|region| {
                let range = region.get("range").unwrap();
                let start = range.get("start").unwrap().as_u64().unwrap() as u32;
                let end = range.get("end").unwrap().as_u64().unwrap() as u32;
                let is_boot_memory = region.get("is_boot_memory").unwrap().as_bool().unwrap();
                let sector_size = region.get("sector_size").unwrap().as_u64().unwrap() as u32;
                let page_size = region.get("page_size").unwrap().as_u64().unwrap() as u32;
                let erased_byte_value = region.get("erased_byte_value").unwrap().as_u64().unwrap() as u8;

                quote::quote!{
                    MemoryRegion::Flash(FlashRegion {
                        range: #start..#end,
                        is_boot_memory: #is_boot_memory,
                        sector_size: #sector_size,
                        page_size: #page_size,
                        erased_byte_value: #erased_byte_value,
                    })
                }
            })
        })
    }).collect()
}

/// Extracts a chip token streams from a yaml value.
fn extract_chip(chip: &serde_yaml::Value) -> (String, proc_macro2::TokenStream) {
    // Extract all the algorithms into a Vec of TokenStreams.
    let algorithms = extract_algorithms(&chip);

    // Extract all the memory regions into a Vec of TookenStreams.
    let memory_map = extract_memory_map(&chip);

    let name = chip.get("name").unwrap().as_str().unwrap().to_owned();
    let manufacturer = quote_option(chip.get("manufacturer").map(|v| v.as_str()));
    let part = quote_option(chip.get("part").map(|v| v.as_str()));
    let core = chip.get("core").unwrap().as_str().unwrap();

    // Quote the chip.
    let chip = quote::quote! {
        Chip {
            name: #name.to_owned(),
            manufacturer: #manufacturer,
            part: #part,
            flash_algorithms: vec![
                #(#algorithms,)*
            ],
            memory_map: vec![
                #(#memory_map,)*
            ],
            core: #core.to_owned(),
        }
    };

    (name, chip)
}