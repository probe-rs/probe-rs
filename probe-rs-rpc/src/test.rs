use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

use crate::flash::BootInfo;
use crate::semihosting_options::SemihostingOptions;
use crate::{Key, RpcResult, RttClient, Session};

#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct Tests {
    pub version: u32,
    pub tests: Vec<Test>,
}

impl From<TestDefinitions> for Tests {
    fn from(def: TestDefinitions) -> Self {
        Self {
            version: def.version,
            tests: def.tests.into_iter().map(Test::from).collect(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TestDefinitions {
    pub version: u32,
    pub tests: Vec<TestDefinition>,
}

#[derive(PartialEq, Debug, Clone, Copy, Serialize, Deserialize, Schema)]
pub enum TestOutcome {
    Panic,
    Pass,
}

#[derive(Debug, Clone, Serialize, Deserialize, Schema, PartialEq)]
pub struct Test {
    pub name: String,
    pub expected_outcome: TestOutcome,
    pub ignored: bool,
    pub timeout: Option<u32>,
    pub address: Option<u32>,
}

impl From<TestDefinition> for Test {
    fn from(def: TestDefinition) -> Self {
        Self {
            name: def.name,
            expected_outcome: def.expected_outcome,
            ignored: def.ignored,
            timeout: def.timeout,
            address: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TestDefinition {
    pub name: String,
    #[serde(
        rename = "should_panic",
        deserialize_with = "outcome_from_should_panic"
    )]
    pub expected_outcome: TestOutcome,
    pub ignored: bool,
    pub timeout: Option<u32>,
}

fn outcome_from_should_panic<'de, D>(deserializer: D) -> Result<TestOutcome, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let should_panic = bool::deserialize(deserializer)?;
    Ok(if should_panic {
        TestOutcome::Panic
    } else {
        TestOutcome::Pass
    })
}

#[derive(Serialize, Deserialize, Schema)]
pub enum TestResult {
    Success,
    Failed(String),
    Cancelled,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct ListTestsRequest {
    pub sessid: Key<Session>,
    pub boot_info: BootInfo,
    /// RTT client if used.
    pub rtt_client: Option<Key<RttClient>>,
    pub semihosting_options: SemihostingOptions,
}

pub type ListTestsResponse = RpcResult<Tests>;

#[derive(Serialize, Deserialize, Schema)]
pub struct RunTestRequest {
    pub sessid: Key<Session>,
    pub test: Test,
    /// RTT client if used.
    pub rtt_client: Option<Key<RttClient>>,
    pub semihosting_options: SemihostingOptions,
}

pub type RunTestResponse = RpcResult<TestResult>;

#[derive(Serialize, Deserialize, Schema)]
pub struct TestKickoffRequest {
    pub sessid: Key<Session>,
    pub core: u32,
    pub address: u64,
}

pub type TestKickoffResponse = RpcResult<()>;
