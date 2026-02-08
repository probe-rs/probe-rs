use fxprof_processed_profile as fxprofpp;
use object::Object;

use super::samply_object;
use super::{CoreSamples, FunctionAddress};

impl FunctionAddress {
    fn to_fxprofpp_with_category(
        self: &FunctionAddress,
        category: fxprofpp::CategoryHandle,
    ) -> fxprofpp::FrameInfo {
        let frame = match self {
            Self::ProgramCounter(addr) => fxprofpp::Frame::InstructionPointer(*addr),
            Self::AdjustedReturnAddress(addr) => fxprofpp::Frame::AdjustedReturnAddress(*addr),
        };

        fxprofpp::FrameInfo {
            frame,
            category_pair: category.into(),
            flags: fxprofpp::FrameFlags::empty(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum MakeFxProfileError {
    #[error("Could not canonicalize ELF file path")]
    Canonicalize(#[source] std::io::Error),
    #[error("Invalid UTF-8 in ELF absolute file path")]
    InvalidUtf8,
    #[error("File name not found for ELF file")]
    NoFileStem,
    #[error("Could not generate debug ID for ELF")]
    DebugId,
}

pub(crate) fn make_fx_profile<'data>(
    core_callstacks: &[CoreSamples],
    start_time: &std::time::SystemTime,
    sampling_interval: &std::time::Duration,
    binary_path: &std::path::Path,
    obj: &impl Object<'data>,
) -> Result<fxprofpp::Profile, MakeFxProfileError> {
    let start_timestamp = (*start_time).into();

    let abs_binary_path: String = binary_path
        .canonicalize()
        .map_err(MakeFxProfileError::Canonicalize)?
        .to_str()
        .ok_or(MakeFxProfileError::InvalidUtf8)?
        .to_owned();

    let binary_name: String = binary_path
        .file_stem()
        .ok_or(MakeFxProfileError::NoFileStem)?
        .to_str()
        .expect("Abs path converted to UTF-8 so file stem should too")
        .to_owned();

    let mut profile =
        fxprofpp::Profile::new(&binary_name, start_timestamp, (*sampling_interval).into());

    let category = profile.add_category("raw", fxprofpp::CategoryColor::Yellow);

    let process = profile.add_process(
        "process",
        0,
        fxprofpp::Timestamp::from_nanos_since_reference(0),
    );

    let debug_id = samply_object::debug_id_for_object(obj).ok_or(MakeFxProfileError::DebugId)?;
    let code_id = samply_object::code_id_for_object(obj);

    let library_info = fxprofpp::LibraryInfo {
        name: binary_name.clone(),
        debug_name: binary_name.clone(),
        path: abs_binary_path.clone(),
        debug_path: abs_binary_path.clone(),
        debug_id,
        code_id: code_id.map(|id| id.to_string()),
        arch: None,
        symbol_table: None,
    };
    let library = profile.add_lib(library_info);

    let start_avma = samply_object::relative_address_base(obj);
    profile.add_lib_mapping(process, library, start_avma, u64::MAX, 0);

    for CoreSamples { core, callstacks } in core_callstacks.iter() {
        let thread = profile.add_thread(
            process,
            *core as u32,
            fxprofpp::Timestamp::from_nanos_since_reference(0),
            false,
        );
        for sample in callstacks {
            let stack_frames = sample
                .callstack
                .iter()
                .map(|frame| frame.to_fxprofpp_with_category(category));
            let stack = profile.intern_stack_frames(thread, stack_frames);
            profile.add_sample(
                thread,
                fxprofpp::Timestamp::from_nanos_since_reference(sample.time.as_nanos() as u64),
                stack,
                fxprofpp::CpuDelta::ZERO,
                1,
            );
        }
    }

    Ok(profile)
}

pub(crate) fn save_fx_profile(
    profile: &fxprofpp::Profile,
    output_dir: &std::path::Path,
    profile_name: &str,
) -> std::io::Result<()> {
    let output_path = output_dir.join(profile_name).with_extension("json.gz");
    let output_file = std::fs::File::create(output_path)?;

    const GZIP_COMPRESSION_LEVEL: u32 = 2;

    let writer = std::io::BufWriter::new(output_file);
    let builder = flate2::GzBuilder::new().filename(profile_name.as_bytes());
    let gz = builder.write(writer, flate2::Compression::new(GZIP_COMPRESSION_LEVEL));
    let gz = std::io::BufWriter::new(gz);
    serde_json::to_writer(gz, &profile)?;
    Ok(())
}

#[cfg(test)]
mod test {
    use super::super::CallstackSample;
    use super::super::test::{addresses_to_callstack, get_path_for_test_files};
    use super::*;

    #[test]
    fn test_make_fx_profile() {
        let executable_name = "esp32c6_coredump_elf";
        let executable_location =
            get_path_for_test_files(format!("debug-unwind-tests/{executable_name}.elf").as_str());

        let object_bytes = std::fs::read(&executable_location).unwrap();
        let obj = object::File::parse(object_bytes.as_slice()).unwrap();

        let input_addresses = [
            0x4200124e, // rust_begin_unwind
            0x420054f2, // _ZN4core9panicking9panic_fmt17h021b089f2ed24437E
            0x42000202, // _ZN16embassy_executor3raw20TaskStorage$LT$F$GT$4poll17hcf2d0b9f6da05190E
            0x420052ec, // _ZN16embassy_executor3raw8Executor4poll17h95bc77c9558ed726E
            0x42000244, // _ZN15esp_hal_embassy8executor6thread8Executor3run17h70decec90d969805E
            0x42000510, // main
            0x4200438c, // hal_main
            0x42000132, // _start_rust
        ];
        let callstack = addresses_to_callstack(&input_addresses);

        let core_callstacks = [CoreSamples {
            core: 0,
            callstacks: vec![CallstackSample {
                callstack,
                time: std::time::Duration::from_secs(7),
            }],
        }];

        let fx_profile = make_fx_profile(
            &core_callstacks,
            &std::time::SystemTime::now(),
            &std::time::Duration::from_secs(7),
            &executable_location,
            &obj,
        )
        .unwrap();

        let profile_value = serde_json::to_value(&fx_profile).unwrap();

        let result_addresses: Vec<u64> = profile_value.get("threads").unwrap()[0]
            .get("frameTable")
            .unwrap()
            .get("address")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap())
            .collect();

        let start_avma = samply_object::relative_address_base(&obj);

        // check output profile has the addresses we put into it
        for input_address in &input_addresses {
            assert!(result_addresses.contains(&(input_address - start_avma)));
        }

        let result_product_name = profile_value
            .get("meta")
            .unwrap()
            .get("product")
            .unwrap()
            .as_str()
            .unwrap();
        assert_eq!(result_product_name, executable_name);
    }
}
