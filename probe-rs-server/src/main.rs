use probe_rs_jsonrpc::server::Server;
use probe_rs_jsonrpc::definitions::ProbeRsGenServer;
use jsonrpc_ws_server::*;
use probe_rs::Probe;
use jsonrpc_core::IoHandler;

fn main() {
    let probe = Probe::list_all()[0].open().unwrap();
    let session = probe.attach("nrf52").unwrap();
    let mut io = IoHandler::new();
    let server = Server::new(session);
    io.extend_with(server.to_delegate());
    let server = ServerBuilder::new(io)
        .start(&"0.0.0.0:3030".parse().unwrap())
        .expect("Server must start with no issues");
    server.wait().unwrap();
}

