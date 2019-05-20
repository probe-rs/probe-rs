/// Standard ROM table entries for Cortex-M Processors
/// 
/// 

mod cortex_m0 {
    use crate::romtable::{
        PeripheralID,
        ComponentModification,
    };

    use jep106::JEP106Code;

    pub const ROM_TABLE_ID: PeripheralID = PeripheralID {
        CMOD: ComponentModification::No,
        JEP106: Some(JEP106Code { id: 0x3b, cc: 0x04 }),
        REVAND: 0,
        REVISION: 0,
        PART: 117,
        SIZE: 1,
    };

    #[test]
    fn cortex_m0_rom_table_id() {
        assert_eq!(PeripheralID::from_raw(&[0x71,0xb4,0xb,0x0,0x4,0x0,0x0,0x0]), ROM_TABLE_ID);
    }
}