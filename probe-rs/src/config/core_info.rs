use std::borrow::Cow;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CoreInfo {
    #[serde(rename = "arm")]
    Arm(ArmCore),
    #[serde(rename = "riscv")]
    RiscV(RiscVCore),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArmCore {
    pub name: Cow<'static, str>,
    #[serde(rename = "type")]
    pub kind: ArmCoreKind,
    pub dp: u8,
    pub ap: u8,
    pub base_address: Option<u32>,
}

impl Default for ArmCore {
    fn default() -> Self {
        Self {
            name: Cow::Borrowed("Core"),
            kind: ArmCoreKind::default(),
            dp: 0,
            ap: 0,
            base_address: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiscVCore {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ArmCoreKind {
    CortexM0,
    CortexM0Plus,
    CortexM1,
    CortexM3,
    CortexM4,
    CortexM7,
    CortexM23,
    CortexM33,
    CortexM35P,
    CortexM55,
    SC000,
    SC300,
    ARMV8MBL,
    ARMV8MML,
    CortexR4,
    CortexR5,
    CortexR7,
    CortexR8,
    CortexA5,
    CortexA7,
    CortexA8,
    CortexA9,
    CortexA15,
    CortexA17,
    CortexA32,
    CortexA35,
    CortexA53,
    CortexA57,
    CortexA72,
    CortexA73,
    Any,
}

impl Default for ArmCoreKind {
    fn default() -> Self {
        ArmCoreKind::Any
    }
}
