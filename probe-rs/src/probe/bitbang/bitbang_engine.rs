use std::io;
use std::net::TcpStream;

use super::bitbang_adapter::BitBangAdapter;

/// Uses a `BitBangAdapter` to provide generic JTAG functions
#[derive(Debug)]
pub struct BitBangEngine {
    pub adapter: BitBangAdapter,
}

impl BitBangEngine {
    pub fn new(socket: TcpStream) -> io::Result<Self> {
        let adapter = BitBangAdapter::new(socket)?;
        Ok(Self { adapter })
    }

    /// Clocks out tms, tdi and clock in tdo. Starts on a falling edge.  TDI will be sampled on the
    /// rising edge, TDO is sampled after the falling edge
    fn clock(&mut self, tms: bool, tdi: bool) -> io::Result<bool> {
        // drop TCK and write our TDI
        self.adapter.write(false, tms, tdi)?;
        // TODO read TDO
        let tdo = self.adapter.read()?;
        // Now clock high again to finish the cycle
        self.adapter.write(true, tms, tdi)?;

        Ok(tdo)
    }

    /// clock out the provided tms bits
    fn write_tms(&mut self, tmss: &[bool]) -> io::Result<()> {
        for tms in tmss {
            self.clock(*tms, false)?;
        }
        Ok(())
    }

    /// Called after entering the shift-ir/dr state.  This function will write the tdi into the
    /// register, and transition to the exit state
    fn shift_reg(&mut self, tdis: &[bool]) -> io::Result<Vec<bool>> {
        let mut tdos = vec![];

        // Nothing to write...
        if tdis.is_empty() {
            return Ok(tdos);
        }

        // We skip the last bit as it will be clocked in when we transition tms
        for tdi in &tdis[..tdis.len() - 1] {
            // TMS will always be low
            let tdo = self.clock(false, *tdi)?;
            tdos.push(tdo)
        }

        // this will transition to the exit state and clock in our last TDI bit
        let tdo = self.clock(true, tdis[tdis.len() - 1])?;
        tdos.push(tdo);

        Ok(tdos)
    }

    /// idle for given number of clock cycles
    pub fn idle(&mut self, clock_cycles: usize) -> io::Result<()> {
        // should already be in the idle state
        for _ in 0..clock_cycles {
            self.clock(false, false)?;
        }
        Ok(())
    }

    /// perform a reset of the tap controller and/or the system, the return to the RunTestIdle
    /// state
    pub fn reset(&mut self, tap_reset: bool, system_reset: bool) -> io::Result<()> {
        self.adapter.reset(tap_reset, system_reset)?;

        // incase trst is not supported, also use the state transitions to get there
        // 5 tms '1's are guarenteed to hit the TestLogicReset state
        self.write_tms(&[true, true, true, true, true])?;
        // transition to RunTestIdleState
        self.write_tms(&[false])?;
        Ok(())
    }

    /// Write data into IR and transiton back to the idle state.  Jtag tap controller must already
    /// be in the RunTestIdle state.
    pub fn write_ir(&mut self, data: &[bool]) -> io::Result<()> {
        // transition to Shift-IR
        self.write_tms(&[true, true, false, false])?;
        // this will leave us in the exit state
        self.shift_reg(data)?;

        // transiton to Update-IR, then RunTestIdle
        self.write_tms(&[true, false])?;
        Ok(())
    }

    /// Write data into DR and transiton back to the idle state.  Jtag tap controller must already
    /// be in the RunTestIdle state.
    pub fn write_read_dr(&mut self, data: &[bool]) -> io::Result<Vec<bool>> {
        // transition to Shift-DR
        self.write_tms(&[true, false, false])?;
        // this will leave us in the exit state
        let read_data = self.shift_reg(data)?;

        // transiton to Update-DR, then RunTestIdle
        self.write_tms(&[true, false])?;

        Ok(read_data)
    }

    /// Write the data into register and return back the data that was shifted out
    pub fn write_read_register(
        &mut self,
        address: u32,
        ir_len: u32,
        data: &[bool],
    ) -> io::Result<Vec<bool>> {
        // convert address to slice of bool
        let mut ir_bools = vec![];
        // the address is still only u32 so it will just be padded up with zeros
        for i in 0..ir_len {
            if i < 32 {
                ir_bools.push((address & (1 << i)) != 0);
            } else {
                ir_bools.push(false);
            }
        }
        self.write_ir(&ir_bools)?;

        self.write_read_dr(data)
    }

    /// Tells the bitbang interface we are done
    pub fn quit(&mut self) -> io::Result<()> {
        self.adapter.quit()?;
        Ok(())
    }
}

// Probably optional...
impl Drop for BitBangEngine {
    fn drop(&mut self) {
        // We are dropping the object, don't care if the socket connection failed
        let _ = self.adapter.quit();
    }
}

/// convert the bool slice to a u8 slice, with each bool being one bit
pub fn bool_slice_to_u8_slice(r: &[bool], len: usize) -> Vec<u8> {
    // split into 8 bit chunks
    // let r = r.chunks(8);

    let mut read_data = vec![];
    // first populate with all zeros
    let bytes = len / 8 + (len % 8 != 0) as usize;
    read_data.resize(bytes, 0u8);

    // now fill in the actual data
    for i in 0..len {
        let bit = r[i] as u8;
        let bit = bit << (i % 8);
        read_data[i / 8] |= bit;
    }

    read_data
}
