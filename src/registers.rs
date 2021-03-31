use std::collections::{btree_map, BTreeMap};

use gimli::{read::CfaRule, EndianSlice, LittleEndian, Register, RegisterRule};
use probe_rs::{Core, CoreRegisterAddress, MemoryInterface};

pub const LR: CoreRegisterAddress = CoreRegisterAddress(14);
pub const PC: CoreRegisterAddress = CoreRegisterAddress(15);
pub const SP: CoreRegisterAddress = CoreRegisterAddress(13);

pub const LR_END: u32 = 0xFFFF_FFFF;

/// Cache and track the state of CPU registers while the stack is being unwound.
pub struct Registers<'c, 'probe> {
    cache: BTreeMap<u16, u32>,
    pub core: &'c mut Core<'probe>,
}

impl<'c, 'probe> Registers<'c, 'probe> {
    pub fn new(lr: u32, sp: u32, core: &'c mut Core<'probe>) -> Self {
        let mut cache = BTreeMap::new();
        cache.insert(LR.0, lr);
        cache.insert(SP.0, sp);
        Self { cache, core }
    }

    pub fn get(&mut self, reg: CoreRegisterAddress) -> anyhow::Result<u32> {
        Ok(match self.cache.entry(reg.0) {
            btree_map::Entry::Occupied(entry) => *entry.get(),
            btree_map::Entry::Vacant(entry) => *entry.insert(self.core.read_core_reg(reg)?),
        })
    }

    pub fn insert(&mut self, reg: CoreRegisterAddress, val: u32) {
        self.cache.insert(reg.0, val);
    }

    /// Updates the Canonical Frame Address (CFA), e.g.
    /// the value of the Stack Pointer (SP) on function entry – the current frame we're looking at
    ///
    /// returns `true` if the CFA has changed
    pub fn update_cfa(
        &mut self,
        rule: &CfaRule<EndianSlice<LittleEndian>>,
    ) -> anyhow::Result</* cfa_changed: */ bool> {
        match rule {
            CfaRule::RegisterAndOffset { register, offset } => {
                let cfa = (i64::from(self.get(gimli2probe(register))?) + offset) as u32;
                let old_cfa = self.cache.get(&SP.0);
                let changed = old_cfa != Some(&cfa);
                if changed {
                    log::debug!("update_cfa: CFA changed {:8x?} -> {:8x}", old_cfa, cfa);
                }
                self.cache.insert(SP.0, cfa);
                Ok(changed)
            }
            // NOTE not encountered in practice so far
            CfaRule::Expression(_) => todo!("CfaRule::Expression"),
        }
    }

    pub fn update(
        &mut self,
        reg: &Register,
        rule: &RegisterRule<EndianSlice<LittleEndian>>,
    ) -> anyhow::Result<()> {
        match rule {
            RegisterRule::Offset(offset) => {
                let cfa = self.get(SP)?;
                let addr = (cfa as i64 + offset) as u32;
                self.cache.insert(reg.0, self.core.read_word_32(addr)?);
            }
            RegisterRule::Undefined => unreachable!(),
            _ => unimplemented!(),
        }
        Ok(())
    }
}

fn gimli2probe(reg: &Register) -> CoreRegisterAddress {
    CoreRegisterAddress(reg.0)
}
