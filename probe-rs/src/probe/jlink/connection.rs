#[derive(Clone, Copy, Debug)]
pub struct JlinkConnection {
    /// Handle
    pub handle: u16,
    /// Process ID
    pub pid: u32,
    /// Host ID
    pub hid: [u8; 4],
    /// IID - unknown
    pub iid: u8,
    /// CID - unknown
    pub cid: u8,
}

impl JlinkConnection {
    pub fn usb(handle: u16) -> Self {
        Self {
            handle,
            pid: 0,
            hid: [0; 4],
            iid: 0,
            cid: 0,
        }
    }

    pub(crate) fn into_bytes(self) -> [u8; 12] {
        [
            self.pid as u8,
            (self.pid >> 8) as u8,
            (self.pid >> 16) as u8,
            (self.pid >> 24) as u8,
            self.hid[0],
            self.hid[1],
            self.hid[2],
            self.hid[3],
            self.iid,
            self.cid,
            self.handle as u8,
            (self.handle >> 8) as u8,
        ]
    }
}
