use crate::probe::batch::Handle;

pub(crate) trait SwdProbe {
    fn dp_write(&mut self, addr: usize, data: u32) -> impl Handle;
    fn dp_read(&mut self, addr: usize) -> impl Handle;
}

pub(crate) enum SwdError {
    Wait,
    Fault,
    NoAck,
}
