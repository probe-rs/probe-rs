use postcard_rpc::header::VarHeader;
use probe_rs_debug::TypedPath;
use probe_rs_rpc::breakpoints::{
    BreakpointResolution, ResolveSourceBreakpointsRequest, ResolveSourceBreakpointsResponse,
    ResolveSourceLocationsRequest, ResolveSourceLocationsResponse,
};

use crate::rpc::functions::RpcContext;

/// Reported per breakpoint when the session was attached without a program
/// binary, so no DWARF is available to resolve source locations against.
const NO_DEBUG_INFO: &str = "No debug information is loaded for this session.";

pub async fn resolve_source_breakpoints(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: ResolveSourceBreakpointsRequest,
) -> ResolveSourceBreakpointsResponse {
    let debug_info = ctx
        .with_server_debug_state(request.sessid, |state| state.debug_info.clone())
        .await;

    // Source breakpoints cannot be placed without DWARF. Report that per
    // breakpoint so the client can show them as unverified.
    let Some(debug_info) = debug_info else {
        return Ok(request
            .locations
            .into_iter()
            .map(|_| BreakpointResolution {
                breakpoint: None,
                error: Some(NO_DEBUG_INFO.to_string()),
            })
            .collect());
    };

    Ok(request
        .locations
        .into_iter()
        .map(|location| {
            match debug_info.get_breakpoint_location(
                TypedPath::derive(location.path.as_bytes()),
                location.line,
                location.column,
            ) {
                Ok(breakpoint) => BreakpointResolution {
                    breakpoint: Some(convert::to_wire_verified_breakpoint(breakpoint)),
                    error: None,
                },
                Err(error) => BreakpointResolution {
                    breakpoint: None,
                    error: Some(error.to_string()),
                },
            }
        })
        .collect())
}

pub async fn resolve_source_locations(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: ResolveSourceLocationsRequest,
) -> ResolveSourceLocationsResponse {
    let debug_info = ctx
        .with_server_debug_state(request.sessid, |state| state.debug_info.clone())
        .await;

    let Some(debug_info) = debug_info else {
        return Ok(request.addresses.iter().map(|_| None).collect());
    };

    Ok(request
        .addresses
        .into_iter()
        .map(|address| {
            debug_info
                .get_source_location(address)
                .as_ref()
                .map(convert::to_wire_source_location)
        })
        .collect())
}

pub(crate) mod convert {
    use probe_rs_debug::{ColumnType, SourceLocation, TypedPath, VerifiedBreakpoint};
    use probe_rs_rpc::breakpoints::{WireColumn, WireSourceLocation, WireVerifiedBreakpoint};

    pub(crate) fn to_wire_source_location(location: &SourceLocation) -> WireSourceLocation {
        WireSourceLocation {
            path: location.path.to_path().display().to_string(),
            line: location.line,
            column: location.column.map(|column| match column {
                ColumnType::LeftEdge => WireColumn::LeftEdge,
                ColumnType::Column(column) => WireColumn::Column(column),
            }),
            address: location.address,
        }
    }

    pub(crate) fn from_wire_source_location(location: WireSourceLocation) -> SourceLocation {
        SourceLocation {
            path: TypedPath::derive(location.path.as_bytes()).to_path_buf(),
            line: location.line,
            column: location.column.map(|column| match column {
                WireColumn::LeftEdge => ColumnType::LeftEdge,
                WireColumn::Column(column) => ColumnType::Column(column),
            }),
            address: location.address,
        }
    }

    pub(crate) fn to_wire_verified_breakpoint(
        breakpoint: VerifiedBreakpoint,
    ) -> WireVerifiedBreakpoint {
        WireVerifiedBreakpoint {
            address: breakpoint.address,
            source_location: to_wire_source_location(&breakpoint.source_location),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn source_location_wire_conversion_preserves_breakpoint_metadata() {
            let location = SourceLocation {
                path: TypedPath::derive(b"C:\\src\\main.rs").to_path_buf(),
                line: Some(42),
                column: Some(ColumnType::LeftEdge),
                address: Some(0x1234),
            };

            let wire = to_wire_source_location(&location);
            assert_eq!(wire.column, Some(WireColumn::LeftEdge));
            assert_eq!(wire.address, Some(0x1234));

            let round_trip = from_wire_source_location(wire);
            assert_eq!(round_trip, location);
        }
    }
}
