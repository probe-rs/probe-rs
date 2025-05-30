pub mod request;
pub mod response;

use serde::de::DeserializeOwned;
use serde_json::Value;

use super::debug_adapter::dap::dap_types::*;

macro_rules! define_request {
    ($endpoint:literal,$request:ty, $args:ty, $response:ty, $result:ty) => {
        impl RequestData for $request {
            type Args = $args;
            type Response = $response;
            type Result = $result;

            const ENDPOINT: &str = $endpoint;

            fn new(seq: i64, arguments: $args) -> Self {
                Self {
                    arguments,
                    command: $endpoint.into(),
                    seq,
                    type_: "request".into(),
                }
            }
        }

        impl Sendable for $request {
            fn seq(&self) -> Option<i64> {
                Some(self.seq)
            }

            fn to_message(self) -> super::debug_adapter::codec::Message {
                super::debug_adapter::codec::Message::Request(Request {
                    arguments: Some(serde_json::to_value(&self).unwrap()),
                    command: self.command,
                    seq: self.seq,
                    type_: self.type_,
                })
            }
        }
    };
}

pub trait Sendable: Send + Sync + 'static {
    fn seq(&self) -> Option<i64>;
    fn to_message(self) -> super::debug_adapter::codec::Message;
}

pub trait RequestData: Sendable + std::fmt::Debug {
    type Args;
    type Response: DeserializeOwned;
    type Result: DeserializeOwned;

    const ENDPOINT: &str;

    fn new(seq: i64, arguments: Self::Args) -> Self;
}

define_request!(
    "breakpointLocation",
    BreakpointLocationsRequest,
    Option<BreakpointLocationsArguments>,
    BreakpointLocationsResponse,
    BreakpointLocationsResponseBody
);

define_request!(
    "runInTerminal",
    RunInTerminalRequest,
    RunInTerminalRequestArguments,
    RunInTerminalResponse,
    RunInTerminalResponseBody
);

define_request!(
    "startDebugging",
    StartDebuggingRequest,
    StartDebuggingRequestArguments,
    StartDebuggingResponse,
    Option<Value>
);

macro_rules! define_event {
    ($name:literal,$event:ty, $body:ty) => {
        impl EventData for $event {
            type Body = $body;

            const NAME: &str = $name;

            fn new(body: $body) -> Self {
                Self {
                    body,
                    event: $name.into(),
                    seq: 0,
                    type_: "event".into(),
                }
            }
        }

        impl Sendable for $event {
            fn seq(&self) -> Option<i64> {
                None
            }

            fn to_message(self) -> super::debug_adapter::codec::Message {
                super::debug_adapter::codec::Message::Event(Event {
                    body: Some(serde_json::to_value(&self).unwrap()),
                    event: self.event,
                    seq: self.seq,
                    type_: self.type_,
                })
            }
        }
    };
}

pub trait EventData: Sendable + std::fmt::Debug {
    type Body;

    const NAME: &str;

    fn new(body: Self::Body) -> Self;
}

define_event!("breakpoint", BreakpointEvent, BreakpointEventBody);
