use crate::rpc::{
    Key,
    functions::{RpcContext, RpcResult},
};
use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

/// Reported per breakpoint when the session was attached without a program
/// binary, so no DWARF is available to resolve source locations against.
const NO_DEBUG_INFO: &str = "No debug information is loaded for this session.";

#[derive(Clone, Debug, Serialize, Deserialize, Schema, PartialEq, Eq)]
pub enum WireColumn {
    LeftEdge,
    Column(u64),
}

#[derive(Clone, Debug, Serialize, Deserialize, Schema, PartialEq, Eq)]
pub struct WireSourceLocation {
    pub path: String,
    pub line: Option<u64>,
    pub column: Option<WireColumn>,
    pub address: Option<u64>,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct SourceBreakpointLocation {
    pub path: String,
    pub line: u64,
    pub column: Option<u64>,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct ResolveSourceBreakpointsRequest {
    pub sessid: Key<Session>,
    pub locations: Vec<SourceBreakpointLocation>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct WireVerifiedBreakpoint {
    pub address: u64,
    pub source_location: WireSourceLocation,
}

#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct BreakpointResolution {
    pub breakpoint: Option<WireVerifiedBreakpoint>,
    pub error: Option<String>,
}

pub type ResolveSourceBreakpointsResponse = RpcResult<Vec<BreakpointResolution>>;

#[derive(Serialize, Deserialize, Schema)]
pub struct ResolveSourceLocationsRequest {
    pub sessid: Key<Session>,
    pub addresses: Vec<u64>,
}

pub type ResolveSourceLocationsResponse = RpcResult<Vec<Option<WireSourceLocation>>>;

use postcard_rpc::header::VarHeader;
use probe_rs::Session;
use probe_rs_debug::TypedPath;

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
                    breakpoint: Some(breakpoint.into()),
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
                .map(WireSourceLocation::from)
        })
        .collect())
}

pub(crate) mod convert {
    use super::{WireColumn, WireSourceLocation, WireVerifiedBreakpoint};
    use probe_rs_debug::{ColumnType, SourceLocation, TypedPath, VerifiedBreakpoint};

    impl From<&SourceLocation> for WireSourceLocation {
        fn from(location: &SourceLocation) -> Self {
            Self {
                path: location.path.to_path().display().to_string(),
                line: location.line,
                column: location.column.map(|column| match column {
                    ColumnType::LeftEdge => WireColumn::LeftEdge,
                    ColumnType::Column(column) => WireColumn::Column(column),
                }),
                address: location.address,
            }
        }
    }

    impl From<WireSourceLocation> for SourceLocation {
        fn from(location: WireSourceLocation) -> Self {
            Self {
                path: TypedPath::derive(location.path.as_bytes()).to_path_buf(),
                line: location.line,
                column: location.column.map(|column| match column {
                    WireColumn::LeftEdge => ColumnType::LeftEdge,
                    WireColumn::Column(column) => ColumnType::Column(column),
                }),
                address: location.address,
            }
        }
    }

    impl From<VerifiedBreakpoint> for WireVerifiedBreakpoint {
        fn from(breakpoint: VerifiedBreakpoint) -> Self {
            Self {
                address: breakpoint.address,
                source_location: WireSourceLocation::from(&breakpoint.source_location),
            }
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

            let wire = WireSourceLocation::from(&location);
            assert_eq!(wire.column, Some(WireColumn::LeftEdge));
            assert_eq!(wire.address, Some(0x1234));

            let round_trip = SourceLocation::from(wire);
            assert_eq!(round_trip, location);
        }
    }
}
