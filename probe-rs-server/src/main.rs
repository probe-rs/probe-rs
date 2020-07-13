use jsonrpc_core::IoHandler;
use jsonrpc_ws_server::*;
use probe_rs::Probe;
use probe_rs_jsonrpc::definitions::ProbeRsGenServer;
use probe_rs_jsonrpc::server::Server;

fn main() {
    let probe = Probe::list_all()[0].open().unwrap();
    let session = probe.attach("nrf52").unwrap();
    let mut io = IoHandler::new();
    let server = Server::new();
    io.extend_with(server.to_delegate());
    let server = ServerBuilder::new(io)
        .start(&"0.0.0.0:3030".parse().unwrap())
        .expect("Server must start with no issues");
    server.wait().unwrap();
}
