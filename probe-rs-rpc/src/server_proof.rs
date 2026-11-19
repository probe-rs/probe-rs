//! A server proves that it knows the token of the client.
//!
//! The login proves the client to the server, and nothing proves the server to the client. A client
//! that gives the server access to its files needs this proof.

use postcard_schema::Schema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha512};

use crate::RpcResult;

#[derive(Serialize, Deserialize, Schema)]
pub struct ServerProofRequest {
    pub nonce: Vec<u8>,
}

pub type ServerProofResponse = RpcResult<Vec<u8>>;

/// Calculates the proof.
///
/// `peer_is_loopback` is `true` if the server sees the client connect from its own host, as
/// through `ssh -W`. A relay to another server that has the same token connects from another
/// host, so its proof does not match.
///
/// A fake server chooses the challenge of the login, and gets the hash of that challenge and the
/// token. It cannot turn that hash into a proof: the proof also hashes a nonce that the client
/// chooses after the login, and the challenge must be a header value, which cannot hold the
/// fixed text and the length fields.
pub fn server_proof(nonce: &[u8], challenge: &str, peer_is_loopback: bool, token: &str) -> Vec<u8> {
    let mut hasher = Sha512::new();
    hasher.update(b"probe-rs server proof");
    hasher.update((nonce.len() as u32).to_le_bytes());
    hasher.update(nonce);
    hasher.update((challenge.len() as u32).to_le_bytes());
    hasher.update(challenge.as_bytes());
    hasher.update([u8::from(peer_is_loopback)]);
    hasher.update(token.as_bytes());
    hasher.finalize().to_vec()
}
