use probe_rs::MemoryInterface;

use crate::DebugError;

/// Frame record contents for 32-bit cores
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRecord32 {
    /// Content of frame or stack pointer part of frame record
    pub frame_pointer: u32,
    pub return_address: u32,
}

/// Frame record contents for 64-bit cores
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRecord64 {
    /// Content of frame or stack pointer part of frame record
    pub frame_pointer: u64,
    pub return_address: u64,
}

/// Attempt to read ARM A32 frame record
pub fn read_arm32_frame_record(
    memory: &mut dyn MemoryInterface,
    frame_pointer: u32,
) -> Result<FrameRecord32, DebugError> {
    let mut frame_record = [0; 2];
    memory.read_32(frame_pointer as u64, &mut frame_record)?;

    let [caller_fp, return_address] = frame_record;

    Ok(FrameRecord32 {
        frame_pointer: caller_fp,
        return_address,
    })
}

/// Attempt to read ARM A64 frame record
pub fn read_arm64_frame_record(
    memory: &mut dyn MemoryInterface,
    frame_pointer: u64,
) -> Result<FrameRecord64, DebugError> {
    let mut frame_record = [0; 2];
    memory.read_64(frame_pointer, &mut frame_record)?;

    let [caller_fp, return_address] = frame_record;

    Ok(FrameRecord64 {
        frame_pointer: caller_fp,
        return_address,
    })
}

/// Attempt to read RV32 frame record
pub fn read_riscv32_frame_record(
    memory: &mut dyn MemoryInterface,
    frame_pointer: u32,
) -> Result<FrameRecord32, DebugError> {
    if frame_pointer < 8 {
        // Frame pointer is too low, cannot read next frame record.
        return Err(DebugError::Other(
            "Stack pointer is too low to unwind".to_string(),
        ));
    }

    let mut frame_record = [0; 2];
    memory.read_32((frame_pointer - 8) as u64, &mut frame_record)?;

    let [caller_fp, return_address] = frame_record;

    Ok(FrameRecord32 {
        frame_pointer: caller_fp,
        return_address,
    })
}

/// Attempt to read Xtensa frame record
pub fn read_xtensa_frame_record(
    memory: &mut dyn MemoryInterface,
    frame_pointer: u32,
) -> Result<FrameRecord32, DebugError> {
    if frame_pointer < 16 {
        // Frame pointer is too low, cannot read next frame record.
        return Err(DebugError::Other(
            "Frame pointer is too low to unwind".to_string(),
        ));
    }

    let mut frame_record = [0; 2];
    memory.read_32((frame_pointer - 16) as u64, &mut frame_record)?;

    // ra and fp are the other way round for Xtensa
    let [return_address, caller_fp] = frame_record;

    Ok(FrameRecord32 {
        frame_pointer: caller_fp,
        return_address,
    })
}
