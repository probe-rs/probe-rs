pub(crate) fn copy_to_buf(data: &[u8], buf: &mut [u8]) -> usize {
    let len = data.len();
    let buf = &mut buf[..len];
    buf.copy_from_slice(data);
    len
}

pub(crate) fn copy_range_to_buf(data: &[u8], offset: u64, length: usize, buf: &mut [u8]) -> usize {
    let offset = match usize::try_from(offset) {
        Ok(v) => v,
        Err(_) => return 0,
    };
    let len = data.len();
    let data = &data[len.min(offset)..len.min(offset + length)];
    copy_to_buf(data, buf)
}
