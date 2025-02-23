use probe_rs::{CoreType, InstructionSet};

use super::TargetDescription;

#[test]
fn test_target_description_microbit() {
    let target_desc = TargetDescription::new(CoreType::Armv6m, InstructionSet::Thumb2);
    let description = target_desc.get_target_xml();

    insta::assert_snapshot!(description);
}

#[test]
fn test_target_with_features() {
    let mut target_desc = TargetDescription::new(CoreType::Armv6m, InstructionSet::Thumb2);
    target_desc.add_gdb_feature("org.probe-rs.feature1");
    target_desc.add_register_from_details("r0", 32, 0.into());
    target_desc.add_register_from_details("x1", 64, 1.into());
    target_desc.add_register_from_details("t2", 64, 2.into());

    target_desc.update_register_name("t2", "at2");
    target_desc.update_register_type("at2", "special_reg");

    target_desc.add_gdb_feature("org.probe-rs.feature2");
    target_desc.add_register_from_details("v4", 128, 4.into());

    let description = target_desc.get_target_xml();

    insta::assert_snapshot!(description);
}
