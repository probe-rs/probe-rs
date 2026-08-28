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
    expected.types == actual.types
        && endpoint_keys(expected) == endpoint_keys(actual)
        && topic_keys(&expected.topics_in) == topic_keys(&actual.topics_in)
        && topic_keys(&expected.topics_out) == topic_keys(&actual.topics_out)
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
    let mut keys: Vec<_> = report
        .endpoints
        .iter()
        .map(|endpoint| (endpoint.path.as_str(), endpoint.req_key, endpoint.resp_key))
        .collect();
    keys.sort_unstable();
    keys
}

fn topic_keys(topics: &[TopicReport]) -> Vec<(&str, Key)> {
    let mut keys: Vec<_> = topics
        .iter()
        .map(|topic| (topic.path.as_str(), topic.key))
        .collect();
    keys.sort_unstable();
    keys
}

fn schema_build_error() -> ClientError {
    ClientError::Transport(TransportError::Message(
        "Failed to build the client RPC schema".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::expected_schema_report;

    #[test]
    fn client_schema_report_resolves() {
        expected_schema_report().expect("client ICD maps must resolve to named types");
    }
}
