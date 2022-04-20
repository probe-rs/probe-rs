use probe_rs::debug::{debug_info::DebugInfo, DebugError};

type TestResult = Result<(), DebugError>;

#[test]
fn function_name_of_inlined_function_1() -> TestResult {
    let di = DebugInfo::from_file("tests/inlined-function").unwrap();

    let address = 0x15e;

    let expected_name = "blink_on";

    let name = di.function_name(address, true)?.unwrap();

    assert_eq!(expected_name, name);

    Ok(())
}

#[test]
fn name_of_function_containing_inlined_function_1() -> TestResult {
    let di = DebugInfo::from_file("tests/inlined-function").unwrap();

    let address = 0x15e;

    let expected_name = "__cortex_m_rt_main";

    let name = di.function_name(address, false)?.unwrap();

    assert_eq!(expected_name, name);

    Ok(())
}

#[test]
fn function_name_of_inlined_function_2() -> TestResult {
    let di = DebugInfo::from_file("tests/inlined-function").unwrap();

    let address = 0x154;

    let expected_name = "__cortex_m_rt_main";

    let name = di.function_name(address, true)?.unwrap();

    assert_eq!(expected_name, name);

    Ok(())
}

#[test]
fn name_of_function_containing_inlined_function_2() -> TestResult {
    let di = DebugInfo::from_file("tests/inlined-function").unwrap();

    let address = 0x154;

    let expected_name = "__cortex_m_rt_main";

    let name = di.function_name(address, false)?.unwrap();

    assert_eq!(expected_name, name);

    Ok(())
}

#[test]
fn function_name_of_non_inlined_function() -> TestResult {
    let di = DebugInfo::from_file("tests/inlined-function").unwrap();

    let address = 0xf4;

    let expected_name = "blink_off";

    let name = di.function_name(address, true)?.unwrap();

    assert_eq!(expected_name, name);

    // The function is not inlined, so we should receive the same name in both cases
    let name = di.function_name(address, false)?.unwrap();
    assert_eq!(expected_name, name);

    Ok(())
}
