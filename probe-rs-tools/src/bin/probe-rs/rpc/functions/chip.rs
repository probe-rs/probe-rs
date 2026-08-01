use postcard_rpc::header::VarHeader;

use probe_rs_rpc::chip::{
    ChipInfoRequest, ChipInfoResponse, ListFamiliesResponse, LoadChipFamilyRequest,
};

use crate::rpc::functions::{RpcContext, convert::lift};
use probe_rs_rpc::NoResponse;

pub async fn list_families(
    ctx: &mut RpcContext,
    _header: VarHeader,
    _req: (),
) -> ListFamiliesResponse {
    Ok(ctx
        .registry()
        .await
        .families()
        .iter()
        .map(convert::to_wire_chip_family)
        .collect())
}

pub async fn chip_info(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: ChipInfoRequest,
) -> ChipInfoResponse {
    Ok(convert::to_wire_chip_data(lift(
        ctx.registry().await.get_target_by_name(request.name),
    )?))
}

pub async fn load_chip_family(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: LoadChipFamilyRequest,
) -> NoResponse {
    lift(
        ctx.registry()
            .await
            .add_target_family_from_yaml(&request.families_yaml),
    )?;

    Ok(())
}

pub(crate) mod convert {
    use probe_rs::Target;
    use probe_rs_rpc::chip::{
        Chip, ChipData, ChipFamily, Core, CoreType, GenericRegion, JEP106Code, MemoryAccess,
        MemoryRegion, NvmRegion, RamRegion,
    };

    pub(crate) fn to_wire_jep106_code(value: jep106::JEP106Code) -> JEP106Code {
        JEP106Code {
            id: value.id,
            cc: value.cc,
        }
    }

    pub(crate) fn from_wire_jep106_code(value: JEP106Code) -> jep106::JEP106Code {
        jep106::JEP106Code {
            id: value.id,
            cc: value.cc,
        }
    }

    pub(crate) fn to_wire_chip_family(value: &probe_rs_target::ChipFamily) -> ChipFamily {
        ChipFamily {
            name: value.name.clone(),
            manufacturer: value.manufacturer.map(to_wire_jep106_code),
            variants: value.variants.iter().map(to_wire_chip).collect(),
        }
    }

    pub(crate) fn to_wire_chip(value: &probe_rs_target::Chip) -> Chip {
        Chip {
            name: value.name.clone(),
        }
    }

    pub(crate) fn to_wire_chip_data(value: Target) -> ChipData {
        ChipData {
            cores: value.cores.into_iter().map(to_wire_core).collect(),
            memory_map: value
                .memory_map
                .into_iter()
                .map(to_wire_memory_region)
                .collect(),
        }
    }

    pub(crate) fn to_wire_core(value: probe_rs_target::Core) -> Core {
        Core {
            name: value.name,
            core_type: to_wire_core_type(value.core_type),
        }
    }

    pub(crate) fn to_wire_core_type(value: probe_rs_target::CoreType) -> CoreType {
        match value {
            probe_rs_target::CoreType::Armv6m => CoreType::Armv6m,
            probe_rs_target::CoreType::Armv7a => CoreType::Armv7a,
            probe_rs_target::CoreType::Armv7r => CoreType::Armv7r,
            probe_rs_target::CoreType::Armv7m => CoreType::Armv7m,
            probe_rs_target::CoreType::Armv7em => CoreType::Armv7em,
            probe_rs_target::CoreType::Armv8a => CoreType::Armv8a,
            probe_rs_target::CoreType::Armv8m => CoreType::Armv8m,
            probe_rs_target::CoreType::Riscv => CoreType::Riscv,
            probe_rs_target::CoreType::Riscv64 => CoreType::Riscv64,
            probe_rs_target::CoreType::Xtensa => CoreType::Xtensa,
        }
    }

    pub(crate) fn to_wire_memory_region(value: probe_rs_target::MemoryRegion) -> MemoryRegion {
        match value {
            probe_rs_target::MemoryRegion::Ram(rr) => MemoryRegion::Ram(to_wire_ram_region(rr)),
            probe_rs_target::MemoryRegion::Generic(gr) => {
                MemoryRegion::Generic(to_wire_generic_region(gr))
            }
            probe_rs_target::MemoryRegion::Nvm(nr) => MemoryRegion::Nvm(to_wire_nvm_region(nr)),
        }
    }

    pub(crate) fn to_wire_nvm_region(value: probe_rs_target::NvmRegion) -> NvmRegion {
        NvmRegion {
            name: value.name,
            range: (value.range.start, value.range.end),
            cores: value.cores,
            is_alias: value.is_alias,
            access: value.access.map(to_wire_memory_access),
        }
    }

    pub(crate) fn to_wire_ram_region(value: probe_rs_target::RamRegion) -> RamRegion {
        RamRegion {
            name: value.name,
            range: (value.range.start, value.range.end),
            cores: value.cores,
            access: value.access.map(to_wire_memory_access),
        }
    }

    pub(crate) fn to_wire_generic_region(value: probe_rs_target::GenericRegion) -> GenericRegion {
        GenericRegion {
            name: value.name,
            range: (value.range.start, value.range.end),
            cores: value.cores,
            access: value.access.map(to_wire_memory_access),
        }
    }

    pub(crate) fn to_wire_memory_access(value: probe_rs_target::MemoryAccess) -> MemoryAccess {
        MemoryAccess {
            read: value.read,
            write: value.write,
            execute: value.execute,
            boot: value.boot,
        }
    }
}
