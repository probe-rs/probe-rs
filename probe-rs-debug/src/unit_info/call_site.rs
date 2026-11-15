//! Collects the call site parameters that `DW_OP_entry_value` needs.

use std::collections::HashMap;

use crate::debug_info::*;
use gimli::AttributeValue;

use super::UnitInfo;

impl UnitInfo {
    pub(crate) fn call_site_value(
        &self,
        return_pc: u64,
        location: &gimli::Expression<GimliReader>,
    ) -> Option<gimli::Expression<GimliReader>> {
        let location = expression_bytes(location)?;
        let params = self
            .call_sites
            .get(&return_pc)
            .or_else(|| self.call_sites.get(&(return_pc & !1)))?;
        params
            .iter()
            .find(|param| param.location == location)
            .map(|param| param.value.clone())
    }
}

pub(super) struct CallSiteParameter {
    location: Vec<u8>,
    value: gimli::Expression<GimliReader>,
}

pub(super) fn collect_call_site(
    die: &gimli::DebuggingInformationEntry<GimliReader>,
    dwarf: &gimli::Dwarf<GimliReader>,
    unit: &gimli::Unit<GimliReader>,
    active_call_site: &mut Option<(isize, u64)>,
    call_sites: &mut HashMap<u64, Vec<CallSiteParameter>>,
) {
    let tag = die.tag();
    if tag == gimli::DW_TAG_call_site || tag == gimli::DW_TAG_GNU_call_site {
        *active_call_site = call_site_return_pc(die, dwarf, unit).map(|pc| (die.depth(), pc));
        return;
    }

    let Some((site_depth, pc)) = *active_call_site else {
        return;
    };

    if die.depth() <= site_depth {
        *active_call_site = None;
        return;
    }

    if die.depth() != site_depth + 1 {
        return;
    }

    if tag != gimli::DW_TAG_call_site_parameter && tag != gimli::DW_TAG_GNU_call_site_parameter {
        return;
    }

    let Some(param) = call_site_parameter(die) else {
        return;
    };
    call_sites.entry(pc).or_default().push(param);
}

fn call_site_return_pc(
    die: &gimli::DebuggingInformationEntry<GimliReader>,
    dwarf: &gimli::Dwarf<GimliReader>,
    unit: &gimli::Unit<GimliReader>,
) -> Option<u64> {
    die_address(die, dwarf, unit, gimli::DW_AT_call_return_pc)
        .or_else(|| die_address(die, dwarf, unit, gimli::DW_AT_low_pc))
        .or_else(|| die_address(die, dwarf, unit, gimli::DW_AT_call_pc))
}

fn die_address(
    die: &gimli::DebuggingInformationEntry<GimliReader>,
    dwarf: &gimli::Dwarf<GimliReader>,
    unit: &gimli::Unit<GimliReader>,
    attr: gimli::DwAt,
) -> Option<u64> {
    match die.attr_value(attr)? {
        AttributeValue::Addr(address) => Some(address),
        AttributeValue::DebugAddrIndex(index) => dwarf.address(unit, index).ok(),
        AttributeValue::Udata(value) => Some(value),
        _ => None,
    }
}

fn call_site_parameter(
    die: &gimli::DebuggingInformationEntry<GimliReader>,
) -> Option<CallSiteParameter> {
    let location = exprloc(die, gimli::DW_AT_location)?;
    let value = exprloc(die, gimli::DW_AT_call_value)
        .or_else(|| exprloc(die, gimli::DW_AT_GNU_call_site_value))?;
    Some(CallSiteParameter {
        location: expression_bytes(&location)?,
        value,
    })
}

fn exprloc(
    die: &gimli::DebuggingInformationEntry<GimliReader>,
    attr: gimli::DwAt,
) -> Option<gimli::Expression<GimliReader>> {
    match die.attr_value(attr)? {
        AttributeValue::Exprloc(expression) => Some(expression),
        _ => None,
    }
}

fn expression_bytes(expression: &gimli::Expression<GimliReader>) -> Option<Vec<u8>> {
    gimli::Reader::to_slice(&expression.0)
        .ok()
        .map(|bytes| bytes.to_vec())
}
