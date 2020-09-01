#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Core {
    Arm(ArmCore),
    RiscV(RiscVCore),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArmCore {
    pub name: Cow<'static, str>
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiscVCore {

}