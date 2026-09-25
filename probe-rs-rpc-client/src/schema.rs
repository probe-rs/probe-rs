use postcard_rpc::Key;
use postcard_rpc::host_client::{HostErr, SchemaError, SchemaReport, TopicReport};
use postcard_rpc::standard_icd::WireError;
use postcard_schema::schema::owned::OwnedNamedType;
use probe_rs_rpc::{ENDPOINT_LIST, TOPICS_IN_LIST, TOPICS_OUT_LIST};

use crate::{ClientError, TransportError, from_host_err};

/// Schema the client implements, including postcard-rpc standard endpoints
/// and topics that the `endpoints!` / `topics!` macros merge in.
pub fn expected_schema_report() -> Result<SchemaReport, ClientError> {
    let mut report = SchemaReport::default();

    for ty in ENDPOINT_LIST
        .types
        .iter()
        .chain(TOPICS_IN_LIST.types)
        .chain(TOPICS_OUT_LIST.types)
    {
        report.add_type(OwnedNamedType::from(*ty));
    }

    for (path, req_key, resp_key) in ENDPOINT_LIST.endpoints {
        report
            .add_endpoint((*path).into(), *req_key, *resp_key)
            .map_err(|_| schema_build_error())?;
    }

    for (path, key) in TOPICS_IN_LIST.topics {
        report
            .add_topic_in((*path).into(), *key)
            .map_err(|_| schema_build_error())?;
    }

    for (path, key) in TOPICS_OUT_LIST.topics {
        report
            .add_topic_out((*path).into(), *key)
            .map_err(|_| schema_build_error())?;
    }

    Ok(report)
}

/// Compare the schema of the client with the schema of the server.
///
/// [`SchemaReport`] stores a named type on each endpoint and topic by
/// searching the type set for a matching key. Two types can hash to the same
/// key, so that lookup is not unique. The type set together with the path and
/// key of each endpoint and topic is the schema on the wire.
pub fn schema_reports_match(expected: &SchemaReport, actual: &SchemaReport) -> bool {
    let types_match = log_type_mismatch(expected, actual);
    let endpoints_match = log_list_mismatch(
        "endpoints",
        &endpoint_keys(expected),
        &endpoint_keys(actual),
    );
    let topics_in_match = log_list_mismatch(
        "inbound topics",
        &topic_keys(&expected.topics_in),
        &topic_keys(&actual.topics_in),
    );
    let topics_out_match = log_list_mismatch(
        "outbound topics",
        &topic_keys(&expected.topics_out),
        &topic_keys(&actual.topics_out),
    );

    types_match && endpoints_match && topics_in_match && topics_out_match
}

fn log_type_mismatch(expected: &SchemaReport, actual: &SchemaReport) -> bool {
    if expected.types == actual.types {
        return true;
    }

    let only_client = formatted_types(expected.types.difference(&actual.types));
    let only_server = formatted_types(actual.types.difference(&expected.types));
    tracing::debug!(
        only_client = ?only_client,
        only_server = ?only_server,
        "RPC type set of the server does not match this client"
    );
    false
}

fn formatted_types<'a>(types: impl Iterator<Item = &'a OwnedNamedType>) -> Vec<String> {
    let mut labels: Vec<_> = types.map(ToString::to_string).collect();
    labels.sort();
    labels
}

fn log_list_mismatch<T: PartialEq + core::fmt::Debug>(
    kind: &str,
    expected: &[T],
    actual: &[T],
) -> bool {
    if expected == actual {
        return true;
    }

    let only_client: Vec<_> = expected
        .iter()
        .filter(|item| !actual.contains(item))
        .collect();
    let only_server: Vec<_> = actual
        .iter()
        .filter(|item| !expected.contains(item))
        .collect();

    // If only the order is different, these will be empty.
    if only_client.is_empty() && only_server.is_empty() {
        return true;
    }

    tracing::debug!(
        only_client = ?only_client,
        only_server = ?only_server,
        "RPC {kind} of the server do not match this client"
    );
    false
}

pub fn from_schema_err(error: SchemaError<WireError>) -> ClientError {
    match error {
        SchemaError::Comms(HostErr::Wire(WireError::UnknownKey)) => ClientError::IncompatibleServer,
        SchemaError::Comms(error) => from_host_err(error),
        SchemaError::TaskError | SchemaError::InvalidReportData | SchemaError::LostData => {
            ClientError::IncompatibleServer
        }
    }
}

fn endpoint_keys(report: &SchemaReport) -> Vec<(&str, Key, Key)> {
    report
        .endpoints
        .iter()
        .map(|endpoint| (endpoint.path.as_str(), endpoint.req_key, endpoint.resp_key))
        .collect()
}

fn topic_keys(topics: &[TopicReport]) -> Vec<(&str, Key)> {
    topics
        .iter()
        .map(|topic| (topic.path.as_str(), topic.key))
        .collect()
}

fn schema_build_error() -> ClientError {
    ClientError::Transport(TransportError::Message(
        "Failed to build the client RPC schema".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::{expected_schema_report, schema_reports_match};

    #[test]
    fn client_schema_report_resolves() {
        expected_schema_report().expect("client ICD maps must resolve to named types");
    }

    #[test]
    fn schema_reports_match_identical_reports() {
        let report = expected_schema_report().unwrap();
        assert!(schema_reports_match(&report, &report));
    }

    #[test]
    fn schema_reports_mismatch_when_endpoint_is_missing() {
        let expected = expected_schema_report().unwrap();
        let mut actual = expected.clone();
        actual.endpoints.pop();
        assert!(!schema_reports_match(&expected, &actual));
    }
}
