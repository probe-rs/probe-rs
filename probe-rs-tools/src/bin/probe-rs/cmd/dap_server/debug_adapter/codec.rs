use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

use super::dap::dap_types::{Event, Request, Response};

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

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Frame<T: PartialEq> {
    pub content: T,
}

impl<T: PartialEq> Frame<T> {
    pub(crate) fn new(content: T) -> Self {
        Self { content }
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum Message {
    Request(Request),
    Response(Response),
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

    pub fn set_seq(&mut self, seq: i64) {
        match self {
            Message::Request(req) => req.seq = seq,
            Message::Response(resp) => resp.seq = seq,
            Message::Event(event) => event.seq = seq,
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

impl From<Response> for Message {
    fn from(value: Response) -> Self {
        Self::Response(value)
    }
}

impl From<Request> for Message {
    fn from(value: Request) -> Self {
        Self::Request(value)
    }
}
