use std::marker::PhantomData;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cmd::dap_server::protocol::response::ResponseKind;

use super::dap::dap_types::{Event, Request};

pub mod decoder;
pub mod encoder;

pub(crate) struct DapCodec<T: Serialize + for<'a> Deserialize<'a>> {
    length: Option<usize>,
    header_received: bool,
    _pd: PhantomData<T>,
}

impl<T: Serialize + for<'a> Deserialize<'a> + PartialEq> DapCodec<T> {
    pub(crate) fn new() -> Self {
        Self {
            length: None,
            header_received: false,
            _pd: PhantomData,
        }
    }
}

impl<T: Serialize + for<'a> Deserialize<'a> + PartialEq> Default for DapCodec<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Message {
    Request(Request),
    Response(ResponseKind),
    Event(Event),
}

impl Message {
    pub(crate) fn kind(&self) -> MessageKind {
        match self {
            Message::Request(_) => MessageKind::Request,
            Message::Response(_) => MessageKind::Response,
            Message::Event(_) => MessageKind::Event,
        }
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub(crate) enum MessageKind {
    Request,
    Response,
    Event,
}

impl From<Event> for Message {
    fn from(value: Event) -> Self {
        Self::Event(value)
    }
}

impl From<ResponseKind> for Message {
    fn from(value: ResponseKind) -> Self {
        Self::Response(value)
    }
}

impl From<Request> for Message {
    fn from(value: Request) -> Self {
        Self::Request(value)
    }
}
