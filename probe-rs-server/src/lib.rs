pub mod definitions;
pub mod server;

use crate::definitions::ProbeRsGenServer;
use crate::server::Server;
use jsonrpc_core::IoHandler;
use jsonrpc_ws_server::*;

pub fn run(host: String, port: u16) {
    let mut io = IoHandler::new();
    let server = Server::new();
    io.extend_with(server.to_delegate());
    let server_addr = std::net::SocketAddr::new(host.parse().unwrap(), port);
    let server = ServerBuilder::new(io)
        .start(&server_addr)
        .expect("Server must start with no issues");
    server.wait().unwrap();
}
