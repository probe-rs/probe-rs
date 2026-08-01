# probe-rs-rpc-client

This crate holds the client for the probe-rs RPC interface. Programs that
talk to a `probe-rs serve` server depend on it.

## Usage

Add the crate as a dependency in your `Cargo.toml` file:

```toml
[dependencies]
probe-rs-rpc-client = { version = "0.1", features = ["remote"] }
```

Use `connect` to open a remote session, then call methods on
`SessionInterface` and `CoreInterface`.
