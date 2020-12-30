use std::fs;
use std::fs::{read_dir, read_to_string};
use std::io;
use std::path::{Path, PathBuf};

/// Parse all target description files in the input directory and create
/// a single output file with the Rust source code
/// for all targets.
pub fn run(input_dir: impl AsRef<Path>, output_file: impl AsRef<Path>) {
    // Determine all config files to parse.
    let mut files = vec![];
    visit_dirs(input_dir.as_ref(), &mut files).unwrap();

    let output_file = output_file.as_ref();

    let output_dir = output_file.parent().unwrap();

    let mut configs: Vec<proc_macro2::TokenStream> = vec![];
    for file in files {
        let string = read_to_string(&file).expect(
            "Algorithm definition file could not be read. This is a bug. Please report it.",
        );

        let yaml: Result<serde_yaml::Value, _> = serde_yaml::from_str(&string);

        match yaml {
            Ok(chip) => {
                let chip = extract_chip_family(&chip, output_dir);
                configs.push(chip);
            }
            Err(e) => {
                panic!("Failed to parse target file: {:?} because:\n{}", file, e);
            }
        }
    }

    let include_stream = if configs.is_empty() {
        quote::quote! {}
    } else {
        quote::quote! {
            #[allow(unused_imports)]
            use jep106::JEP106Code;
            use crate::config::{Chip, RawFlashAlgorithm, NvmRegion, MemoryRegion, RamRegion, SectorDescription, FlashProperties};

            use std::borrow::Cow;
        }
    };

    let target_count = configs.len();

    let stream = quote::quote! {
        #include_stream
        use crate::config::ChipFamily;

        #[allow(clippy::unreadable_literal)]
        pub const TARGETS: [ChipFamily;#target_count] = [
            #(#configs,)*
        ];

        pub fn get_targets() -> &'static [ChipFamily] {
            &TARGETS
        }
    };

    fs::write(output_file, stream.to_string()).expect("Writing build.rs output failed.");
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
fn extract_algorithms(
    chip: &serde_yaml::Value,
    output_dir: &Path,
) -> Vec<proc_macro2::TokenStream> {
    // Get an iterator over all the algorithms contained in the chip value obtained from the yaml file.
    let algorithm_iter = chip
        .get("flash_algorithms")
        .into_iter()
        .flat_map(|f| f.as_mapping().into_iter().flat_map(|f2| f2.iter()));

    algorithm_iter
        .map(|(_name, algorithm)| {
            // Extract all values and form them into a struct.
            let name = algorithm
                .get("name")
                .unwrap()
                .as_str()
                .unwrap()
                .to_ascii_lowercase();
            let description = algorithm
                .get("description")
                .unwrap()
                .as_str()
                .unwrap()
                .to_ascii_lowercase();
            let default = algorithm.get("default").unwrap().as_bool().unwrap();
            let instructions: Vec<u8> =
                base64::decode(algorithm.get("instructions").unwrap().as_str().unwrap()).unwrap();
            let pc_init =
                quote_option(algorithm.get("pc_init").unwrap().as_u64().map(|v| v as u32));
            let pc_uninit = quote_option(
                algorithm
                    .get("pc_uninit")
                    .unwrap()
                    .as_u64()
                    .map(|v| v as u32),
            );
            let pc_program_page =
                algorithm.get("pc_program_page").unwrap().as_u64().unwrap() as u32;
            let pc_erase_sector =
                algorithm.get("pc_erase_sector").unwrap().as_u64().unwrap() as u32;
            let pc_erase_all = quote_option(
                algorithm
                    .get("pc_erase_all")
                    .unwrap()
                    .as_u64()
                    .map(|v| v as u32),
            );
            let data_section_offset = algorithm
                .get("data_section_offset")
                .unwrap()
                .as_u64()
                .unwrap() as u32;

            let flash_properties = algorithm.get("flash_properties").unwrap();

            let range = flash_properties.get("address_range").unwrap();
            let start = range.get("start").unwrap().as_u64().unwrap() as u32;
            let end = range.get("end").unwrap().as_u64().unwrap() as u32;
            let page_size = flash_properties.get("page_size").unwrap().as_u64().unwrap() as u32;
            let erased_byte_value = flash_properties
                .get("erased_byte_value")
                .unwrap()
                .as_u64()
                .unwrap() as u8;
            let program_page_timeout = flash_properties
                .get("program_page_timeout")
                .unwrap()
                .as_u64()
                .unwrap() as u32;
            let erase_sector_timeout = flash_properties
                .get("erase_sector_timeout")
                .unwrap()
                .as_u64()
                .unwrap() as u32;

            // get all sectors
            let sectors = extract_sectors(&flash_properties);

            // write flash algorithm into separate file

            let mut algorithm_file_name = name.replace(" ", "_");

            algorithm_file_name.push_str(".bin");

            let algorithm_path = output_dir.join(&algorithm_file_name);

            fs::write(&algorithm_path, &instructions).unwrap();

            // Quote the algorithm struct.
            let algorithm = quote::quote! {
                RawFlashAlgorithm {
                    name: Cow::Borrowed(#name),
                    description: Cow::Borrowed(#description),
                    default: #default,
                    instructions: Cow::Borrowed(include_bytes!(#algorithm_file_name)),
                    pc_init: #pc_init,
                    pc_uninit: #pc_uninit,
                    pc_program_page: #pc_program_page,
                    pc_erase_sector: #pc_erase_sector,
                    pc_erase_all: #pc_erase_all,
                    data_section_offset: #data_section_offset,
                    flash_properties: FlashProperties {
                        address_range: #start..#end,
                        page_size: #page_size,
                        erased_byte_value: #erased_byte_value,
                        program_page_timeout: #program_page_timeout,
                        erase_sector_timeout: #erase_sector_timeout,
                        sectors: Cow::Borrowed(&[
                            #(#sectors,)*
                        ])
                    },
                }
            };

            algorithm
        })
        .collect()
}

fn extract_sectors(region: &serde_yaml::Value) -> Vec<proc_macro2::TokenStream> {
    match region.get("sectors") {
        Some(sectors) => {
            let iter = sectors.as_sequence().unwrap().iter();

            iter.map(|sector| {
                let size = sector.get("size").unwrap().as_u64().unwrap() as u32;
                let address = sector.get("address").unwrap().as_u64().unwrap() as u32;

                quote::quote! {
                    SectorDescription {
                        size: #size,
                        address: #address,
                    }
                }
            })
            .collect()
        }
        // Currently, sectors might be missing due to the old target generation code
        // For that case, just create a single entry based on the old values
        None => vec![],
    }
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

    memory_map_iter
        .filter_map(|memory_region| {
            // Check if it's a RAM region. If yes, parse it into a TokenStream.
            memory_region
                .get("Ram")
                .map(|region| {
                    let range = region.get("range").unwrap();
                    let start = range.get("start").unwrap().as_u64().unwrap() as u32;
                    let end = range.get("end").unwrap().as_u64().unwrap() as u32;
                    let is_boot_memory = region.get("is_boot_memory").unwrap().as_bool().unwrap();

                    quote::quote! {
                        MemoryRegion::Ram(RamRegion {
                            range: #start..#end,
                            is_boot_memory: #is_boot_memory,
                        })
                    }
                })
                .or_else(|| {
                    memory_region.get("Nvm").map(|region| {
                        let range = region.get("range").unwrap();
                        let start = range.get("start").unwrap().as_u64().unwrap() as u32;
                        let end = range.get("end").unwrap().as_u64().unwrap() as u32;
                        let is_boot_memory =
                            region.get("is_boot_memory").unwrap().as_bool().unwrap();

                        quote::quote! {
                            MemoryRegion::Nvm(NvmRegion {
                                range: #start..#end,
                                is_boot_memory: #is_boot_memory,
                            })
                        }
                    })
                })
        })
        .collect()
}

/// Extracts a list of algorithm token streams from a yaml value.
fn extract_variants(chip_family: &serde_yaml::Value) -> Vec<proc_macro2::TokenStream> {
    // Get an iterator over all the algorithms contained in the chip value obtained from the yaml file.
    let variants_iter = chip_family
        .get("variants")
        .unwrap()
        .as_sequence()
        .unwrap()
        .iter();

    variants_iter
        .map(|variant| {
            let name = variant.get("name").unwrap().as_str().unwrap();
            let part = quote_option(
                variant
                    .get("part")
                    .and_then(|v| v.as_u64().map(|v| v as u16)),
            );

            // Extract all the memory regions into a Vec of TookenStreams.
            let memory_map = extract_memory_map(&variant);

            let flash_algorithms = variant
                .get("flash_algorithms")
                .into_iter()
                .flat_map(|f| f.as_sequence().into_iter().flat_map(|f2| f2.iter()));

            let flash_algorithm_names = flash_algorithms.map(|a| a.as_str().unwrap());
            quote::quote! {
                Chip {
                    name: Cow::Borrowed(#name),
                    part: #part,
                    memory_map: Cow::Borrowed(&[
                        #(#memory_map,)*
                    ]),
                    flash_algorithms: Cow::Borrowed(&[
                        #(Cow::Borrowed(#flash_algorithm_names),)*
                    ]),
                }
            }
        })
        .collect()
}

/// Extracts a chip family token stream from a yaml value.
fn extract_chip_family(
    chip_family: &serde_yaml::Value,
    output_dir: &Path,
) -> proc_macro2::TokenStream {
    // Extract all the algorithms into a Vec of TokenStreams.
    let algorithms = extract_algorithms(&chip_family, output_dir);

    // Extract all the available variants into a Vec of TokenStreams.
    let variants = extract_variants(&chip_family);

    let name = chip_family
        .get("name")
        .unwrap()
        .as_str()
        .unwrap()
        .to_ascii_lowercase();
    let core = chip_family
        .get("core")
        .unwrap()
        .as_str()
        .unwrap()
        .to_ascii_lowercase();
    let manufacturer = quote_option(extract_manufacturer(&chip_family));

    // Quote the chip.
    let chip_family = quote::quote! {
        ChipFamily {
            name: Cow::Borrowed(#name),
            manufacturer: #manufacturer,
            flash_algorithms: Cow::Borrowed(&[
                #(#algorithms,)*
            ]),
            variants: Cow::Borrowed(&[
                #(#variants,)*
            ]),
            core: Cow::Borrowed(#core),
        }
    };

    chip_family
}

/// Extracts the jep code token stream from a yaml value.
fn extract_manufacturer(chip: &serde_yaml::Value) -> Option<proc_macro2::TokenStream> {
    chip.get("manufacturer").and_then(|manufacturer| {
        let cc = manufacturer.get("cc").map(|v| v.as_u64().unwrap() as u8);
        let id = manufacturer.get("id").map(|v| v.as_u64().unwrap() as u8);

        // For a valid JEP106 Code we need both cc and id
        if cc.is_some() && id.is_some() {
            Some(quote::quote! {
                JEP106Code {
                    cc: #cc,
                    id: #id,
                }
            })
        } else {
            None
        }
    })
}
