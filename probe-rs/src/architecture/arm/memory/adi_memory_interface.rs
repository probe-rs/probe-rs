use crate::{
    CoreStatus, Error, MemoryInterface,
    architecture::arm::{
        ArmDebugInterface, ArmError, DapAccess, FullyQualifiedApAddress,
        ap::{
            AccessPortType, ApAccess, ApRegister, CSW, DRW, DataSize, TAR, TAR2,
            memory_ap::{MemoryAp, MemoryApType},
        },
        memory::ArmMemoryInterface,
    },
    memory::{Operation, OperationKind},
    probe::DebugProbeError,
};

/// Calculate the maximum number of bytes we can write starting at address
/// before we run into the 10-bit TAR autoincrement limit.
fn autoincr_max_bytes(address: u64) -> usize {
    ((address + 1).next_multiple_of(AUTOINCR_LIMIT) - address) as usize
}

/// How far TAR auto-increments before it has to be written again.
const AUTOINCR_LIMIT: u64 = 0x400;

/// A struct to give access to a targets memory using a certain DAP.
pub(crate) struct ADIMemoryInterface<'interface, APA> {
    interface: &'interface mut APA,
    memory_ap: MemoryAp,
}

impl<'interface, APA> ADIMemoryInterface<'interface, APA>
where
    APA: DapAccess,
{
    /// Creates a new MemoryInterface for given AccessPort.
    pub fn new(
        interface: &'interface mut APA,
        access_port_address: &FullyQualifiedApAddress,
    ) -> Result<ADIMemoryInterface<'interface, APA>, ArmError> {
        let memory_ap = MemoryAp::new(interface, access_port_address)?;
        Ok(Self {
            interface,
            memory_ap,
        })
    }
}

/// The element width of a memory access.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Width {
    U8,
    U16,
    U32,
    U64,
}

impl Width {
    fn bytes(self) -> u64 {
        match self {
            Width::U8 => 1,
            Width::U16 => 2,
            Width::U32 => 4,
            Width::U64 => 8,
        }
    }

    /// A 64-bit element is always two words, least significant first, whether or not the AP can
    /// transfer 64 bits at a time.
    fn drw_words(self) -> usize {
        match self {
            Width::U64 => 2,
            _ => 1,
        }
    }

    /// The transfer size the AP has to be in to carry this width.
    fn data_size(self, large_data_extension: bool) -> DataSize {
        match self {
            Width::U8 => DataSize::U8,
            Width::U16 => DataSize::U16,
            Width::U32 => DataSize::U32,
            Width::U64 if large_data_extension => DataSize::U64,
            // Each of the two words is transferred at 32 bits.
            Width::U64 => DataSize::U32,
        }
    }

    /// How far an element at `address` sits from bit zero of its DRW word.
    ///
    /// ADIv5.2 C2.2.6: a sub-word access travels in its byte lane, so a value whose address is not
    /// a multiple of four is not at the bottom of the word.
    fn lane_shift(self, address: u64) -> u32 {
        match self {
            Width::U8 | Width::U16 => ((address % 4) * 8) as u32,
            Width::U32 | Width::U64 => 0,
        }
    }

    fn mask(self) -> u32 {
        match self {
            Width::U8 => 0xFF,
            Width::U16 => 0xFFFF,
            Width::U32 | Width::U64 => u32::MAX,
        }
    }
}

/// A memory access as the lowering sees it: where it starts, how wide its elements are, and how
/// many there are.
struct Shape {
    address: u64,
    width: Width,
    elements: usize,
}

impl Shape {
    fn new(address: u64, width: Width, elements: usize) -> Self {
        Self {
            address,
            width,
            elements,
        }
    }

    fn element_address(&self, index: usize) -> u64 {
        self.address + index as u64 * self.width.bytes()
    }

    /// How many DRW words the whole access moves.
    fn words(&self) -> usize {
        self.elements * self.width.drw_words()
    }

    /// Pull element `index` out of the DRW word that carried it.
    fn take_lane(&self, word: u32, index: usize) -> u32 {
        (word >> self.width.lane_shift(self.element_address(index))) & self.width.mask()
    }

    /// Put element `index` into the byte lane its address selects.
    fn put_lane(&self, value: u32, index: usize) -> u32 {
        value << self.width.lane_shift(self.element_address(index))
    }
}

/// What one probe batch carries: the operations from the front of a list that can share it.
#[derive(Default)]
struct Lowered {
    accesses: Vec<(u64, Option<u32>)>,
    /// Where each operation's reads land in the reply: the first word, and how many.
    spans: Vec<(usize, usize)>,
    /// How many words the reply holds.
    words: usize,
}

/// Stop gathering, keeping what has been lowered so far.
///
/// The first operation of a batch has nothing to fall back to, so its failure is the caller's. A
/// later one only ends the batch early, and starts the next one.
fn stop(lowered: Lowered, error: ArmError) -> Result<Lowered, ArmError> {
    if lowered.spans.is_empty() {
        return Err(error);
    }
    Ok(lowered)
}

impl<AP> ADIMemoryInterface<'_, AP>
where
    AP: DapAccess,
{
    /// Append the accesses that point the AP at `address`, to prefix the transfers they set up.
    ///
    /// TAR, TAR2 and DRW share a register bank, so one list carries all three.
    fn push_target_address(
        &self,
        accesses: &mut Vec<(u64, Option<u32>)>,
        address: u64,
    ) -> Result<(), ArmError> {
        if self.memory_ap.has_large_address_extension() {
            accesses.push((TAR2::ADDRESS, Some((address >> 32) as u32)));
        } else if address > u32::MAX as u64 {
            return Err(ArmError::OutOfBounds);
        }
        accesses.push((TAR::ADDRESS, Some(address as u32)));
        Ok(())
    }

    /// Put the AP in the transfer size `shape` needs.
    fn set_data_size(&mut self, shape: &Shape) -> Result<(), ArmError> {
        let size = shape
            .width
            .data_size(self.memory_ap.has_large_data_extension());
        self.memory_ap.try_set_datasize(self.interface, size)
    }

    /// The AP register accesses that perform `shape`.
    ///
    /// `word` supplies each written DRW word by element and word index, and returns `None`
    /// throughout for a read. The whole access, auto-increment windows included, comes back as one
    /// list, so the address writes and the transfers they set up reach the probe together.
    fn lower(
        &self,
        shape: &Shape,
        mut word: impl FnMut(usize, usize) -> Option<u32>,
    ) -> Result<Vec<(u64, Option<u32>)>, ArmError> {
        let span = shape.elements as u64 * shape.width.bytes();

        // Every address below is derived from `shape`, so checking the whole span here covers
        // each one the walk produces.
        shape
            .address
            .checked_add(span)
            .ok_or(ArmError::OutOfBounds)?;

        let windows = span.div_ceil(AUTOINCR_LIMIT) as usize;
        let mut accesses = Vec::with_capacity(shape.words() + 2 * windows.max(1));
        let mut done = 0;

        while done < shape.elements {
            // TAR only auto-increments in its low ten bits, so every window needs its own address.
            let address = shape.element_address(done);
            let per_window = autoincr_max_bytes(address) / shape.width.bytes() as usize;
            debug_assert!(per_window > 0, "an aligned element fits its window");
            let chunk = (shape.elements - done).min(per_window);

            self.push_target_address(&mut accesses, address)?;
            for element in done..done + chunk {
                for index in 0..shape.width.drw_words() {
                    accesses.push((DRW::ADDRESS, word(element, index)));
                }
            }

            done += chunk;
        }

        Ok(accesses)
    }

    /// Read `shape` into the DRW words it produces.
    fn run_read(&mut self, shape: &Shape, words: &mut [u32]) -> Result<(), ArmError> {
        self.set_data_size(shape)?;
        let accesses = self.lower(shape, |_, _| None)?;
        self.interface
            .access_raw_ap_registers(self.memory_ap.ap_address(), &accesses, words)
    }

    /// The shape of a memory operation, or `None` for one the lowering does not describe.
    ///
    /// `Read` and `Write` are byte-granular and choose their own widths of their own accord, so
    /// neither has one shape.
    fn shape_of(&self, operation: &Operation<'_>) -> Result<Option<Shape>, ArmError> {
        let (width, elements) = match &operation.operation {
            OperationKind::Read8(data) => (Width::U8, data.len()),
            OperationKind::Read16(data) => (Width::U16, data.len()),
            OperationKind::Read32(data) => (Width::U32, data.len()),
            OperationKind::Read64(data) => (Width::U64, data.len()),
            OperationKind::Write8(data) => (Width::U8, data.len()),
            OperationKind::Write16(data) => (Width::U16, data.len()),
            OperationKind::Write32(data) => (Width::U32, data.len()),
            OperationKind::Write64(data) => (Width::U64, data.len()),
            OperationKind::WriteWord8(_) => (Width::U8, 1),
            OperationKind::WriteWord16(_) => (Width::U16, 1),
            OperationKind::WriteWord32(_) => (Width::U32, 1),
            OperationKind::WriteWord64(_) => (Width::U64, 1),
            OperationKind::Read(_) | OperationKind::Write(_) => return Ok(None),
        };

        let bits = width.bytes() as usize * 8;
        if matches!(width, Width::U8 | Width::U16) && self.memory_ap.supports_only_32bit_data_size()
        {
            return Err(ArmError::UnsupportedTransferWidth(bits));
        }
        if !operation.address.is_multiple_of(width.bytes()) {
            return Err(ArmError::alignment_error(
                operation.address,
                width.bytes() as usize,
            ));
        }

        Ok(Some(Shape::new(operation.address, width, elements)))
    }

    /// The accesses that perform `operation`, whose shape the caller has already worked out.
    fn lower_operation(
        &self,
        operation: &Operation<'_>,
        shape: &Shape,
    ) -> Result<Vec<(u64, Option<u32>)>, ArmError> {
        match &operation.operation {
            OperationKind::Read8(_)
            | OperationKind::Read16(_)
            | OperationKind::Read32(_)
            | OperationKind::Read64(_) => self.lower(shape, |_, _| None),
            OperationKind::Write8(data) => self.lower(shape, |element, _| {
                Some(shape.put_lane(data[element] as u32, element))
            }),
            OperationKind::Write16(data) => self.lower(shape, |element, _| {
                Some(shape.put_lane(data[element] as u32, element))
            }),
            OperationKind::Write32(data) => self.lower(shape, |element, _| Some(data[element])),
            OperationKind::Write64(data) => self.lower(shape, |element, index| {
                let value = data[element];
                Some(if index == 0 {
                    value as u32
                } else {
                    (value >> 32) as u32
                })
            }),
            OperationKind::WriteWord8(value) => {
                let value = *value as u32;
                self.lower(shape, |element, _| Some(shape.put_lane(value, element)))
            }
            OperationKind::WriteWord16(value) => {
                let value = *value as u32;
                self.lower(shape, |element, _| Some(shape.put_lane(value, element)))
            }
            OperationKind::WriteWord32(value) => {
                let value = *value;
                self.lower(shape, |_, _| Some(value))
            }
            OperationKind::WriteWord64(value) => {
                let value = *value;
                self.lower(shape, |_, index| {
                    Some(if index == 0 {
                        value as u32
                    } else {
                        (value >> 32) as u32
                    })
                })
            }
            OperationKind::Read(_) | OperationKind::Write(_) => {
                unreachable!("an unshaped operation is never grouped")
            }
        }
    }

    /// Put the words a batch read back into the operation that asked for them.
    fn scatter(operation: &mut Operation<'_>, words: &[u32]) {
        let address = operation.address;

        match &mut operation.operation {
            OperationKind::Read8(data) => {
                let shape = Shape::new(address, Width::U8, data.len());
                for (index, value) in data.iter_mut().enumerate() {
                    *value = shape.take_lane(words[index], index) as u8;
                }
            }
            OperationKind::Read16(data) => {
                let shape = Shape::new(address, Width::U16, data.len());
                for (index, value) in data.iter_mut().enumerate() {
                    *value = shape.take_lane(words[index], index) as u16;
                }
            }
            OperationKind::Read32(data) => data.copy_from_slice(words),
            OperationKind::Read64(data) => {
                for (index, value) in data.iter_mut().enumerate() {
                    *value = words[index * 2] as u64 | ((words[index * 2 + 1] as u64) << 32);
                }
            }
            _ => {}
        }
    }

    /// Whether `operation` hands words back.
    fn is_read(operation: &Operation<'_>) -> bool {
        matches!(
            operation.operation,
            OperationKind::Read8(_)
                | OperationKind::Read16(_)
                | OperationKind::Read32(_)
                | OperationKind::Read64(_)
        )
    }

    /// The operations from the front of `operations` that can share one batch.
    ///
    /// Gathering stops at an operation the lowering does not describe, at a change of transfer
    /// size, and at anything that fails once the batch already holds something.
    fn gather(&mut self, operations: &[Operation<'_>]) -> Result<Lowered, ArmError> {
        let large = self.memory_ap.has_large_data_extension();
        let mut lowered = Lowered::default();
        let mut batch_size = None;

        for operation in operations {
            let shape = match self.shape_of(operation) {
                Ok(Some(shape)) => shape,
                // A byte-granular access runs on its own.
                Ok(None) => break,
                Err(error) => return stop(lowered, error),
            };

            // Lowered before the size is settled: settling it records a CSW write as sent, so
            // nothing may be recorded for an operation that then turns out not to lower.
            let more = match self.lower_operation(operation, &shape) {
                Ok(more) => more,
                Err(error) => return stop(lowered, error),
            };

            let size = shape.width.data_size(large);
            match batch_size {
                // The AP is put in the size before any of the batch goes out, so a change of width
                // has to wait for the next one.
                None => {
                    if let Err(error) = self.memory_ap.try_set_datasize(self.interface, size) {
                        return stop(lowered, error);
                    }
                    batch_size = Some(size);
                }
                Some(carried) if carried != size => break,
                Some(_) => {}
            }

            let reads = if Self::is_read(operation) {
                shape.words()
            } else {
                0
            };
            lowered.accesses.extend(more);
            lowered.spans.push((lowered.words, reads));
            lowered.words += reads;
        }

        Ok(lowered)
    }

    /// Run a list of memory operations in as few probe transactions as possible.
    ///
    /// Every operation that completes records `Ok`; the list stops at the first failure, whose
    /// index comes back with the error. A batch faults as a whole, so a failure inside one names
    /// the first operation of that batch rather than the one the target refused.
    fn run_operations(
        &mut self,
        operations: &mut [Operation<'_>],
    ) -> Result<(), (usize, ArmError)> {
        let mut start = 0;

        while start < operations.len() {
            // A byte-granular access picks its own widths, so it has no shape and runs alone.
            if matches!(
                operations[start].operation,
                OperationKind::Read(_) | OperationKind::Write(_)
            ) {
                let address = operations[start].address;
                let outcome = match &mut operations[start].operation {
                    OperationKind::Read(data) => self.read(address, data),
                    OperationKind::Write(data) => self.write(address, data),
                    _ => unreachable!("just matched"),
                };
                outcome.map_err(|error| (start, error))?;
                operations[start].result = Some(Ok(()));
                start += 1;
                continue;
            }

            let lowered = self
                .gather(&operations[start..])
                .map_err(|error| (start, error))?;
            let end = start + lowered.spans.len();
            debug_assert!(
                end > start,
                "the first operation always joins its own lowered"
            );

            let mut values = vec![0; lowered.words];
            if let Err(error) = self.interface.access_raw_ap_registers(
                self.memory_ap.ap_address(),
                &lowered.accesses,
                &mut values,
            ) {
                // The batch may have carried a CSW write that never reached the target.
                // Re-read rather than trust the note.
                let _ = self.memory_ap.status(self.interface);
                return Err((start, error));
            }

            for (operation, (first, reads)) in operations[start..end].iter_mut().zip(lowered.spans)
            {
                Self::scatter(operation, &values[first..first + reads]);
                operation.result = Some(Ok(()));
            }
            start = end;
        }

        Ok(())
    }

    /// Write `shape`, taking each DRW word from `word`.
    fn run_write(
        &mut self,
        shape: &Shape,
        word: impl FnMut(usize, usize) -> Option<u32>,
    ) -> Result<(), ArmError> {
        self.set_data_size(shape)?;
        let accesses = self.lower(shape, word)?;
        self.interface
            .access_raw_ap_registers(self.memory_ap.ap_address(), &accesses, &mut [])
    }
}

impl<AP> MemoryInterface<ArmError> for ADIMemoryInterface<'_, AP>
where
    AP: DapAccess,
{
    /// Read a block of 64 bit words at `address`.
    ///
    /// The number of words read is `data.len()`.
    /// The address where the read should be performed at has to be a multiple of 8.
    /// Returns `ArmError::MemoryNotAligned` if this does not hold true.
    fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), ArmError> {
        if data.is_empty() {
            return Ok(());
        }
        if !address.is_multiple_of(8) {
            return Err(ArmError::alignment_error(address, 8));
        }

        let shape = Shape::new(address, Width::U64, data.len());
        let mut words = vec![0; shape.words()];
        self.run_read(&shape, &mut words)?;

        for (index, value) in data.iter_mut().enumerate() {
            *value = words[index * 2] as u64 | ((words[index * 2 + 1] as u64) << 32);
        }

        Ok(())
    }

    /// Read a block of 32 bit words at `address`.
    ///
    /// The number of words read is `data.len()`.
    /// The address where the read should be performed at has to be a multiple of 4.
    /// Returns `ArmError::MemoryNotAligned` if this does not hold true.
    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), ArmError> {
        if data.is_empty() {
            return Ok(());
        }
        if !address.is_multiple_of(4) {
            return Err(ArmError::alignment_error(address, 4));
        }

        // A 32-bit element is one DRW word, so the reply is already what the caller asked for.
        let shape = Shape::new(address, Width::U32, data.len());
        self.run_read(&shape, data)
    }

    /// Read a block of 16 bit words at `address`.
    ///
    /// The number of words read is `data.len()`.
    /// The address where the read should be performed at has to be a multiple of 2.
    /// Returns `ArmError::MemoryNotAligned` if this does not hold true.
    fn read_16(&mut self, address: u64, data: &mut [u16]) -> Result<(), ArmError> {
        if self.memory_ap.supports_only_32bit_data_size() {
            return Err(ArmError::UnsupportedTransferWidth(16));
        }
        if !address.is_multiple_of(2) {
            return Err(ArmError::alignment_error(address, 2));
        }
        if data.is_empty() {
            return Ok(());
        }

        let shape = Shape::new(address, Width::U16, data.len());
        let mut words = vec![0; shape.words()];
        self.run_read(&shape, &mut words)?;

        for (index, value) in data.iter_mut().enumerate() {
            *value = shape.take_lane(words[index], index) as u16;
        }

        Ok(())
    }

    /// Read a block of 8 bit words at `address`.
    ///
    /// The number of words read is `data.len()`.
    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), ArmError> {
        if self.memory_ap.supports_only_32bit_data_size() {
            return Err(ArmError::UnsupportedTransferWidth(8));
        }
        if data.is_empty() {
            return Ok(());
        }

        let shape = Shape::new(address, Width::U8, data.len());
        let mut words = vec![0; shape.words()];
        self.run_read(&shape, &mut words)?;

        for (index, value) in data.iter_mut().enumerate() {
            *value = shape.take_lane(words[index], index) as u8;
        }

        Ok(())
    }

    /// Write a block of 64 bit words at `address`.
    ///
    /// The number of words written is `data.len()`.
    /// The address where the write should be performed at has to be a multiple of 8.
    /// Returns `ArmError::MemoryNotAligned` if this does not hold true.
    fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), ArmError> {
        if !address.is_multiple_of(8) {
            return Err(ArmError::alignment_error(address, 8));
        }
        if data.is_empty() {
            return Ok(());
        }

        let shape = Shape::new(address, Width::U64, data.len());
        self.run_write(&shape, |element, index| {
            let value = data[element];
            Some(if index == 0 {
                value as u32
            } else {
                (value >> 32) as u32
            })
        })
    }

    /// Write a block of 32 bit words at `address`.
    ///
    /// The number of words written is `data.len()`.
    /// The address where the write should be performed at has to be a multiple of 4.
    /// Returns `ArmError::MemoryNotAligned` if this does not hold true.
    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), ArmError> {
        if !address.is_multiple_of(4) {
            return Err(ArmError::alignment_error(address, 4));
        }
        if data.is_empty() {
            return Ok(());
        }

        let shape = Shape::new(address, Width::U32, data.len());
        self.run_write(&shape, |element, _| Some(data[element]))
    }

    /// Write a block of 16 bit words at `address`.
    ///
    /// The number of words written is `data.len()`.
    /// The address where the write should be performed at has to be a multiple of 2.
    /// Returns `ArmError::MemoryNotAligned` if this does not hold true.
    fn write_16(&mut self, address: u64, data: &[u16]) -> Result<(), ArmError> {
        if self.memory_ap.supports_only_32bit_data_size() {
            return Err(ArmError::UnsupportedTransferWidth(16));
        }
        if !address.is_multiple_of(2) {
            return Err(ArmError::alignment_error(address, 2));
        }
        if data.is_empty() {
            return Ok(());
        }

        let shape = Shape::new(address, Width::U16, data.len());
        self.run_write(&shape, |element, _| {
            Some(shape.put_lane(data[element] as u32, element))
        })
    }

    /// Write a block of 8 bit words at `address`.
    ///
    /// The number of words written is `data.len()`.
    fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), ArmError> {
        if self.memory_ap.supports_only_32bit_data_size() {
            return Err(ArmError::UnsupportedTransferWidth(8));
        }
        if data.is_empty() {
            return Ok(());
        }

        let shape = Shape::new(address, Width::U8, data.len());
        self.run_write(&shape, |element, _| {
            Some(shape.put_lane(data[element] as u32, element))
        })
    }

    fn execute_memory_operations(&mut self, operations: &mut [Operation<'_>]) {
        if let Err((index, error)) = self.run_operations(operations) {
            operations[index].result = Some(Err(Error::from(error)));
        }
    }

    /// Flushes any pending commands when the underlying probe interface implements command queuing.
    fn flush(&mut self) -> Result<(), ArmError> {
        self.interface.flush()
    }

    /// True if the memory ap supports 64 bit accesses which might be more efficient than issuing
    /// two 32bit transaction on the device’s memory bus.
    fn supports_native_64bit_access(&mut self) -> bool {
        self.memory_ap.has_large_data_extension()
    }

    fn supports_8bit_transfers(&self) -> Result<bool, ArmError> {
        Ok(!self.memory_ap.supports_only_32bit_data_size())
    }
}

impl<APA> ArmMemoryInterface for ADIMemoryInterface<'_, APA>
where
    APA: ApAccess + ArmDebugInterface,
{
    fn execute_operations(&mut self, operations: &mut [Operation<'_>]) -> Result<(), ArmError> {
        self.run_operations(operations).map_err(|(_, error)| error)
    }

    fn base_address(&mut self) -> Result<u64, ArmError> {
        self.memory_ap.base_address(self.interface)
    }

    fn fully_qualified_address(&self) -> FullyQualifiedApAddress {
        self.memory_ap.ap_address().clone()
    }

    fn get_arm_debug_interface(&mut self) -> Result<&mut dyn ArmDebugInterface, DebugProbeError> {
        Ok(self.interface)
    }

    fn generic_status(&mut self) -> Result<CSW, ArmError> {
        self.memory_ap.generic_status(self.interface)
    }

    fn update_core_status(&mut self, state: CoreStatus) {
        self.interface.core_status_notification(state);
    }
}

#[cfg(test)]
mod tests {
    use scroll::Pread;
    use test_log::test;

    use super::{Shape, Width};
    use crate::{
        MemoryInterface,
        architecture::arm::{
            FullyQualifiedApAddress, ap::memory_ap::mock::MockMemoryAp, memory::ADIMemoryInterface,
        },
    };

    impl<'interface> ADIMemoryInterface<'interface, MockMemoryAp> {
        /// Creates a new MemoryInterface for given AccessPort.
        fn new_mock(
            mock: &'interface mut MockMemoryAp,
        ) -> ADIMemoryInterface<'interface, MockMemoryAp> {
            Self::new(mock, &FullyQualifiedApAddress::v1_with_default_dp(0)).unwrap()
        }

        fn mock_memory(&self) -> &[u8] {
            &self.interface.memory
        }
    }

    // Visually obvious pattern used to test memory writes
    const DATA8: &[u8] = &[
        128, 129, 130, 131, 132, 133, 134, 135, 136, 137, 138, 139, 140, 141, 142, 143,
    ];

    // DATA8 interpreted as little endian 16-bit words
    const DATA16: &[u16] = &[
        0x8180, 0x8382, 0x8584, 0x8786, 0x8988, 0x8b8a, 0x8d8c, 0x8f8e,
    ];

    // DATA8 interpreted as little endian 32-bit words
    const DATA32: &[u32] = &[0x83828180, 0x87868584, 0x8b8a8988, 0x8f8e8d8c];

    #[test]
    fn a_block_access_writes_the_address_once() {
        use crate::architecture::arm::ap::{ApRegister, DRW, TAR};

        let mut mock = MockMemoryAp::with_pattern_and_size(256);
        let mi = ADIMemoryInterface::new_mock(&mut mock);

        let accesses = mi
            .lower(&Shape::new(0x2000_0000, Width::U32, 3), |_, _| None)
            .unwrap();

        assert_eq!(
            accesses,
            vec![
                (TAR::ADDRESS, Some(0x2000_0000)),
                (DRW::ADDRESS, None),
                (DRW::ADDRESS, None),
                (DRW::ADDRESS, None),
            ]
        );
    }

    #[test]
    fn an_access_writes_the_address_again_at_every_auto_increment_window() {
        use crate::architecture::arm::ap::{ApRegister, DRW, TAR};

        // TAR only auto-increments in its low ten bits, so a run that crosses a 1 KiB boundary has
        // to say where it is again.
        let mut mock = MockMemoryAp::with_pattern_and_size(256);
        let mi = ADIMemoryInterface::new_mock(&mut mock);

        let accesses = mi
            .lower(&Shape::new(0x2000_03F8, Width::U32, 3), |_, _| None)
            .unwrap();

        assert_eq!(
            accesses,
            vec![
                (TAR::ADDRESS, Some(0x2000_03F8)),
                (DRW::ADDRESS, None),
                (DRW::ADDRESS, None),
                (TAR::ADDRESS, Some(0x2000_0400)),
                (DRW::ADDRESS, None),
            ]
        );
    }

    #[test]
    fn a_sub_word_value_travels_in_its_byte_lane() {
        // ADIv5.2 C2.2.6: a byte whose address is not a multiple of four is not at bit zero of the
        // word that carries it.
        let shape = Shape::new(0x2000_0001, Width::U8, 2);

        assert_eq!(shape.put_lane(0xAB, 0), 0x0000_AB00);
        assert_eq!(shape.put_lane(0xCD, 1), 0x00CD_0000);
        assert_eq!(shape.take_lane(0x0000_AB00, 0), 0xAB);
        assert_eq!(shape.take_lane(0x00CD_0000, 1), 0xCD);
    }

    #[test]
    fn read_word_32() {
        let mut mock = MockMemoryAp::with_pattern_and_size(256);
        mock.memory[..8].copy_from_slice(&DATA8[..8]);
        let mut mi = ADIMemoryInterface::new_mock(&mut mock);

        for address in [0, 4] {
            let value = mi.read_word_32(address).expect("read_word_32 failed");
            assert_eq!(value, DATA32[address as usize / 4]);
        }
    }

    #[test]
    fn read_word_16() {
        let mut mock = MockMemoryAp::with_pattern_and_size(256);
        mock.memory[..8].copy_from_slice(&DATA8[..8]);
        let mut mi = ADIMemoryInterface::new_mock(&mut mock);

        for address in [0, 2, 4, 6] {
            let value = mi.read_word_16(address).expect("read_word_16 failed");
            assert_eq!(value, DATA16[address as usize / 2]);
        }
    }

    #[test]
    fn read_word_8() {
        let mut mock = MockMemoryAp::with_pattern_and_size(256);
        mock.memory[..8].copy_from_slice(&DATA8[..8]);
        let mut mi = ADIMemoryInterface::new_mock(&mut mock);

        for address in 0..8 {
            let value = mi
                .read_word_8(address)
                .unwrap_or_else(|_| panic!("read_word_8 failed, address = {address}"));
            assert_eq!(value, DATA8[address as usize], "address = {address}");
        }
    }

    #[test]
    fn write_word_32() {
        for address in [0, 4] {
            let mut mock = MockMemoryAp::with_pattern_and_size(256);
            let mut mi = ADIMemoryInterface::new_mock(&mut mock);

            let mut expected = Vec::from(mi.mock_memory());
            expected[address as usize..][..4].copy_from_slice(&DATA8[..4]);

            mi.write_word_32(address, DATA32[0])
                .unwrap_or_else(|_| panic!("write_word_32 failed, address = {address}"));
            assert_eq!(mi.mock_memory(), expected.as_slice(), "address = {address}");
        }
    }

    #[test]
    fn write_word_16() {
        for address in [0, 2, 4, 6] {
            let mut mock = MockMemoryAp::with_pattern_and_size(256);
            let mut mi = ADIMemoryInterface::new_mock(&mut mock);

            let mut expected = Vec::from(mi.mock_memory());
            expected[address as usize..][..2].copy_from_slice(&DATA8[..2]);

            mi.write_word_16(address, DATA16[0])
                .unwrap_or_else(|_| panic!("write_word_32 failed, address = {address}"));
            assert_eq!(mi.mock_memory(), expected.as_slice(), "address = {address}");
        }
    }

    #[test]
    fn write_word_8() {
        for address in 0..8 {
            let mut mock = MockMemoryAp::with_pattern_and_size(256);
            let mut mi = ADIMemoryInterface::new_mock(&mut mock);

            let mut expected = Vec::from(mi.mock_memory());
            expected[address] = DATA8[0];

            mi.write_word_8(address as u64, DATA8[0])
                .unwrap_or_else(|_| panic!("write_word_8 failed, address = {address}"));
            assert_eq!(mi.mock_memory(), expected.as_slice(), "address = {address}");
        }
    }

    #[test]
    fn read_32() {
        let mut mock = MockMemoryAp::with_pattern_and_size(256);
        mock.memory[..DATA8.len()].copy_from_slice(DATA8);
        let mut mi = ADIMemoryInterface::new_mock(&mut mock);

        for address in [0, 4] {
            for len in 0..3 {
                let mut data = vec![0u32; len];
                mi.read_32(address, &mut data)
                    .unwrap_or_else(|_| panic!("read_32 failed, address = {address}, len = {len}"));

                assert_eq!(
                    data.as_slice(),
                    &DATA32[(address / 4) as usize..(address / 4) as usize + len],
                    "address = {address}, len = {len}"
                );
            }
        }
    }

    #[test]
    fn read_32_big_chunk() {
        let mut mock = MockMemoryAp::with_pattern_and_size(4096);
        let expected: Vec<u32> = mock
            .memory
            .chunks(4)
            .map(|b| b.pread(0).unwrap())
            .take(513)
            .collect();
        let mut mi = ADIMemoryInterface::new_mock(&mut mock);

        let mut data = vec![0u32; 513];
        mi.read_32(0, &mut data)
            .unwrap_or_else(|_| panic!("read_32 failed, address = {}, len = {}", 0, data.len()));

        assert_eq!(
            data.as_slice(),
            expected,
            "address = {}, len = {}",
            0,
            data.len()
        );
    }

    #[test]
    fn read_32_unaligned_should_error() {
        let mut mock = MockMemoryAp::with_pattern_and_size(256);
        let mut mi = ADIMemoryInterface::new_mock(&mut mock);

        for address in [1, 3, 127] {
            assert!(mi.read_32(address, &mut [0u32; 4]).is_err());
        }
    }

    #[test]
    fn read_16() {
        let mut mock = MockMemoryAp::with_pattern_and_size(256);
        mock.memory[..DATA8.len()].copy_from_slice(DATA8);
        let mut mi = ADIMemoryInterface::new_mock(&mut mock);

        for address in [0, 2, 4, 6] {
            for len in 0..4 {
                let mut data = vec![0u16; len];
                mi.read_16(address, &mut data)
                    .unwrap_or_else(|_| panic!("read_16 failed, address = {address}, len = {len}"));

                assert_eq!(
                    data.as_slice(),
                    &DATA16[(address / 2) as usize..(address / 2) as usize + len],
                    "address = {address}, len = {len}"
                );
            }
        }
    }

    #[test]
    fn read_8() {
        let mut mock = MockMemoryAp::with_pattern_and_size(256);
        mock.memory[..DATA8.len()].copy_from_slice(DATA8);
        let mut mi = ADIMemoryInterface::new_mock(&mut mock);

        for address in 0..4 {
            for len in 0..12 {
                let mut data = vec![0u8; len];
                mi.read_8(address, &mut data)
                    .unwrap_or_else(|_| panic!("read_8 failed, address = {address}, len = {len}"));

                assert_eq!(
                    data.as_slice(),
                    &DATA8[address as usize..address as usize + len],
                    "address = {address}, len = {len}"
                );
            }
        }
    }

    #[test]
    fn read() {
        let mut mock = MockMemoryAp::with_pattern_and_size(256);
        mock.memory[..DATA8.len()].copy_from_slice(DATA8);
        let mut mi = ADIMemoryInterface::new_mock(&mut mock);

        for address in 0..4 {
            for len in 0..12 {
                let mut data = vec![0u8; len];
                mi.read(address, &mut data)
                    .unwrap_or_else(|_| panic!("read failed, address = {address}, len = {len}"));

                assert_eq!(
                    &DATA8[address as usize..address as usize + len],
                    data.as_slice(),
                    "address = {address}, len = {len}"
                );
            }
        }
    }

    #[test]
    fn write_32() {
        for address in [0, 4] {
            for len in 0..3 {
                let mut mock = MockMemoryAp::with_pattern_and_size(256);
                let mut mi = ADIMemoryInterface::new_mock(&mut mock);

                let mut expected = Vec::from(mi.mock_memory());
                expected[address as usize..(address as usize) + len * 4]
                    .copy_from_slice(&DATA8[..len * 4]);

                let data = &DATA32[..len];
                mi.write_32(address, data).unwrap_or_else(|_| {
                    panic!("write_32 failed, address = {address}, len = {len}")
                });

                assert_eq!(
                    mi.mock_memory(),
                    expected.as_slice(),
                    "address = {address}, len = {len}"
                );
            }
        }
    }

    #[test]
    fn write_16() {
        for address in [0, 2, 4, 6] {
            for len in 0..3 {
                let mut mock = MockMemoryAp::with_pattern_and_size(256);
                let mut mi = ADIMemoryInterface::new_mock(&mut mock);

                let mut expected = Vec::from(mi.mock_memory());
                expected[address as usize..(address as usize) + len * 2]
                    .copy_from_slice(&DATA8[..len * 2]);

                let data = &DATA16[..len];
                mi.write_16(address, data).unwrap_or_else(|_| {
                    panic!("write_16 failed, address = {address}, len = {len}")
                });

                assert_eq!(
                    mi.mock_memory(),
                    expected.as_slice(),
                    "address = {address}, len = {len}"
                );
            }
        }
    }

    #[test]
    fn write_block_u32_unaligned_should_error() {
        let mut mock = MockMemoryAp::with_pattern_and_size(256);
        let mut mi = ADIMemoryInterface::new_mock(&mut mock);

        for address in [1, 3, 127] {
            assert!(mi.write_32(address, &[0xDEAD_BEEF, 0xABBA_BABE]).is_err());
        }
    }

    #[test]
    fn write_8() {
        for address in 0..4 {
            for len in 0..12 {
                let mut mock = MockMemoryAp::with_pattern_and_size(256);
                let mut mi = ADIMemoryInterface::new_mock(&mut mock);

                let mut expected = Vec::from(mi.mock_memory());
                expected[address as usize..(address as usize) + len].copy_from_slice(&DATA8[..len]);

                let data = &DATA8[..len];
                mi.write_8(address, data)
                    .unwrap_or_else(|_| panic!("write_8 failed, address = {address}, len = {len}"));

                assert_eq!(
                    mi.mock_memory(),
                    expected.as_slice(),
                    "address = {address}, len = {len}"
                );
            }
        }
    }

    #[test]
    fn write() {
        for address in 0..4 {
            for len in 0..12 {
                let mut mock = MockMemoryAp::with_pattern_and_size(256);
                let mut mi = ADIMemoryInterface::new_mock(&mut mock);

                let mut expected = Vec::from(mi.mock_memory());
                expected[address as usize..(address as usize) + len].copy_from_slice(&DATA8[..len]);

                let data = &DATA8[..len];
                mi.write(address, data)
                    .unwrap_or_else(|_| panic!("write failed, address = {address}, len = {len}"));

                assert_eq!(
                    mi.mock_memory(),
                    expected.as_slice(),
                    "address = {address}, len = {len}"
                );
            }
        }
    }
}
