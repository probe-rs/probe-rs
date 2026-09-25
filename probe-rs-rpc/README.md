# probe-rs-rpc

This crate holds the wire types, endpoint tables, and transport for the
probe-rs RPC protocol. The `probe-rs-rpc-client` crate and the
`probe-rs serve` server depend on it.

## Version compatibility

The protocol is not stable between releases. On connect, the client fetches
the schema of the server and compares it with the schema of the client. A
mismatch ends the session with `ClientError::IncompatibleServer`. postcard-rpc
also keys each endpoint by a hash of its path and its schemas, so a later
unknown endpoint cannot corrupt data.

## Usage

Add the crate as a dependency in your `Cargo.toml` file:

```toml
[dependencies]
probe-rs-rpc = "0.1"
```

Enable the `remote` feature when you need the unix socket or websocket
transport. Enable the `clap` feature when you need CLI option types.
