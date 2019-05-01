pub mod general;

use scroll;

trait Command: scroll::ctx::TryIntoCtx {
    type Request: Request;
    type Response: Response;
}

trait Request: scroll::ctx::TryIntoCtx {

};

trait Response: scroll::ctx::TryFromCtx {

};

pub fn send_command<C: Command>(command: C, request: C::Request) -> C::Response {
    let mut buffer = [0u8; 1024];

    // Write the command & request to the buffer.
    // TODO: Error handling & real USB writing.
    let size = buffer.pwrite(command, 0)?;
    buffer.pwrite(command, size);

    // Read back resonse.
    // TODO: Error handling & real USB reading.
    buffer.pread(0)
}