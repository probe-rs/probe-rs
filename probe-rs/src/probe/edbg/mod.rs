use log::debug;
use std::sync::Mutex;

use crate::probe::daplink::DAPLink;

pub struct EDBGprobe {
    device: Mutex<DAPLink>
}
