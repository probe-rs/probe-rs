//! Compare backend that uses signature functions of the flash algorithm.
//!
//! A flash algorithm can export `VendorFunc_Signatures_<hash>`. The function signs a table of
//! flash ranges in target RAM, and probe-rs compares each signature with its own.

use std::time::Duration;

use md5::{Digest as _, Md5};

use super::{CompareContext, CompareCost, CompareOutcome, FlashCompare, Outside, SectorCheck};
use crate::config::NvmRegion;
use crate::flashing::{ActiveFlasher, FlashAlgorithm, FlashError, Verify};
use crate::memory::MemoryInterface;

/// A signature algorithm that probe-rs can compute.
#[derive(Debug)]
pub(crate) struct SignatureHash {
    /// The suffix of the function name, after `Signatures_`.
    pub name: &'static str,
    /// The signature size in bytes.
    pub size: usize,
    /// Computes the signature of the data.
    pub compute: fn(&[u8]) -> Vec<u8>,
}

/// The known signature algorithms, in preference order.
pub(crate) const HASHES: &[SignatureHash] = &[SignatureHash {
    name: "md5",
    size: 16,
    compute: |data| Md5::digest(data).to_vec(),
}];

/// The prefix of a signature function in the vendor function map.
const FUNCTION_PREFIX: &str = "Signatures_";

/// The range holds data, and the signature is valid.
const STATUS_SIGNED: u32 = 0;
/// The range is erased. The signature is not used.
const STATUS_EMPTY: u32 = 1;

/// The size of the fields before the signature in a table entry.
const ENTRY_HEADER: usize = 12;

/// The time that a signature function gets for each MiB of flash, after a fixed base.
const TIME_PER_MIB: Duration = Duration::from_secs(1);
const BASE_TIMEOUT: Duration = Duration::from_secs(2);

/// The signature table that a signature function reads and writes.
///
/// Each entry is `address: u32`, `size: u32`, `status: u32`, then the signature, padded to
/// four bytes. All fields are little-endian.
struct SignatureTable;

impl SignatureTable {
    /// Returns the size of one entry.
    pub fn stride(hash: &SignatureHash) -> usize {
        (ENTRY_HEADER + hash.size).next_multiple_of(4)
    }

    /// Returns how many entries fit in `buffer_size` bytes.
    pub fn capacity(hash: &SignatureHash, buffer_size: usize) -> usize {
        buffer_size / Self::stride(hash)
    }

    /// Encodes the `(address, size)` ranges into a table.
    pub fn encode(hash: &SignatureHash, ranges: &[Entry]) -> Vec<u8> {
        let stride = Self::stride(hash);
        let mut table = vec![0; ranges.len() * stride];
        for (entry, Entry { address, size, .. }) in table.chunks_exact_mut(stride).zip(ranges) {
            entry[0..4].copy_from_slice(&address.to_le_bytes());
            entry[4..8].copy_from_slice(&size.to_le_bytes());
        }
        table
    }

    /// Decodes the `(status, signature)` results of a table.
    pub fn decode(hash: &SignatureHash, table: &[u8]) -> impl Iterator<Item = (u32, Vec<u8>)> {
        table.chunks_exact(Self::stride(hash)).map(|entry| {
            let status = u32::from_le_bytes(entry[8..12].try_into().unwrap());
            let signature = entry[ENTRY_HEADER..][..hash.size].to_vec();
            (status, signature)
        })
    }
}

/// One range to sign, with the signature that it must have.
struct Entry {
    check: usize,
    address: u32,
    size: u32,
    expected: Vec<u8>,
    /// The entry covers the full sector, so an empty result means an empty sector.
    full_sector: bool,
    /// The expected content is all erased.
    expected_erased: bool,
}

/// Compares sectors with the signature function of the flash algorithm.
#[derive(Debug)]
pub(crate) struct AlgorithmSignatures {
    hash: &'static SignatureHash,
    function: String,
}

impl AlgorithmSignatures {
    /// Returns the backend for the first known signature function of `algorithm`.
    pub(crate) fn new(algorithm: &FlashAlgorithm) -> Option<Self> {
        for name in algorithm.vendor_functions.keys() {
            if let Some(suffix) = name.strip_prefix(FUNCTION_PREFIX)
                && !HASHES.iter().any(|hash| hash.name == suffix)
            {
                tracing::debug!("Ignoring the unknown signature function {name}");
            }
        }

        HASHES.iter().find_map(|hash| {
            let function = format!("{FUNCTION_PREFIX}{}", hash.name);
            algorithm
                .vendor_functions
                .contains_key(&function)
                .then_some(Self { hash, function })
        })
    }

    /// Returns the name of the hash that the signature function computes.
    pub(crate) fn hash_name(&self) -> &'static str {
        self.hash.name
    }

    /// Sets the result of each check that the signature function can sign.
    pub(crate) fn compare_checks(
        &self,
        active: &mut ActiveFlasher<'_, '_, Verify>,
        checks: &mut [SectorCheck<'_>],
    ) -> Result<(), FlashError> {
        let erased = active.flash_algorithm().flash_properties.erased_byte_value;
        let entries = self.entries(checks, erased);
        let results = self.sign(active, &entries)?;

        for (idx, check) in checks.iter_mut().enumerate() {
            let mut of_check = entries
                .iter()
                .zip(&results)
                .filter(|(entry, _)| entry.check == idx)
                .peekable();
            if of_check.peek().is_none() {
                continue;
            }
            check.outcome = check_outcome(of_check);
        }

        Ok(())
    }

    /// Builds the entries for the checks. A check that cannot be signed gets no entry.
    fn entries(&self, checks: &[SectorCheck<'_>], erased: u8) -> Vec<Entry> {
        let mut entries = Vec::new();
        for (idx, check) in checks.iter().enumerate() {
            let full_sector = matches!(check.outside, Outside::Erased(_));
            let ranges = check.contiguous_ranges();

            let fits = ranges.iter().all(|(address, data)| {
                u32::try_from(*address).is_ok() && u32::try_from(data.len()).is_ok()
            });
            if !fits {
                continue;
            }

            entries.extend(ranges.into_iter().map(|(address, data)| Entry {
                check: idx,
                address: address as u32,
                size: data.len() as u32,
                expected: (self.hash.compute)(&data),
                full_sector,
                expected_erased: data.iter().all(|&byte| byte == erased),
            }));
        }
        entries
    }

    /// Runs the signature function on `entries`, and returns one result for each entry.
    fn sign(
        &self,
        active: &mut ActiveFlasher<'_, '_, Verify>,
        entries: &[Entry],
    ) -> Result<Vec<(u32, Vec<u8>)>, FlashError> {
        let (buffer, buffer_size) = active.scratch_buffer();
        let capacity = SignatureTable::capacity(self.hash, buffer_size);
        if capacity == 0 {
            return Err(FlashError::VendorFunctionFailed {
                name: self.function.clone(),
                source: "The page buffer is too small for one signature table entry".into(),
            });
        }

        let mut results = Vec::with_capacity(entries.len());
        while results.len() < entries.len() {
            let batch = &entries[results.len()..];
            let batch = &batch[..batch.len().min(capacity)];

            let mut table = SignatureTable::encode(self.hash, batch);
            active
                .core
                .write(buffer, &table)
                .map_err(FlashError::Core)?;

            let bytes = batch.iter().map(|entry| entry.size as u64).sum::<u64>();
            let timeout = BASE_TIMEOUT + TIME_PER_MIB * (bytes / (1024 * 1024) + 1) as u32;
            let done = active.call_vendor_function_with_timeout(
                &self.function,
                [Some(buffer), Some(batch.len() as u64), None, None],
                timeout,
            )? as usize;
            if done == 0 || done > batch.len() {
                return Err(FlashError::RoutineCallFailed {
                    name: self.function.clone(),
                    error_code: done as u32,
                });
            }

            let stride = SignatureTable::stride(self.hash);
            active
                .core
                .read(buffer, &mut table[..done * stride])
                .map_err(FlashError::Core)?;
            results.extend(SignatureTable::decode(self.hash, &table[..done * stride]));
        }

        Ok(results)
    }
}

/// Compares `checks` with the signature function of the flash algorithm.
///
/// Sets the result of each check, and returns the name of the hash. Returns `None` if the
/// algorithm has no known signature function.
///
/// Do not use, only exposed with `pub` visibility for target-gen.
pub fn compare_with_signatures(
    active: &mut ActiveFlasher<'_, '_, Verify>,
    checks: &mut [SectorCheck<'_>],
) -> Result<Option<&'static str>, FlashError> {
    let Some(backend) = AlgorithmSignatures::new(active.flash_algorithm()) else {
        return Ok(None);
    };

    backend.compare_checks(active, checks)?;

    Ok(Some(backend.hash_name()))
}

/// Returns the result of a check from the results of its entries.
fn check_outcome<'a>(
    results: impl Iterator<Item = (&'a Entry, &'a (u32, Vec<u8>))>,
) -> Option<CompareOutcome> {
    let mut outcome = CompareOutcome::Match;
    for (entry, (status, signature)) in results {
        match *status {
            STATUS_SIGNED if *signature == entry.expected => {}
            STATUS_SIGNED => return Some(CompareOutcome::Mismatch),
            STATUS_EMPTY if entry.expected_erased => {}
            STATUS_EMPTY if entry.full_sector => outcome = CompareOutcome::Empty,
            STATUS_EMPTY => return Some(CompareOutcome::Mismatch),
            _ => return None,
        }
    }
    Some(outcome)
}

impl FlashCompare for AlgorithmSignatures {
    fn name(&self) -> &str {
        &self.function
    }

    fn cost(&self) -> CompareCost {
        CompareCost::Cheap
    }

    fn compare(
        &self,
        context: &mut CompareContext<'_>,
        _region: &NvmRegion,
        checks: &mut [SectorCheck<'_>],
    ) -> Result<(), FlashError> {
        let mut active = context.algorithm()?;
        self.compare_checks(&mut active, checks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(name: &str) -> &'static SignatureHash {
        HASHES.iter().find(|hash| hash.name == name).unwrap()
    }

    #[test]
    fn md5_test_vector() {
        // RFC 1321, "abc".
        assert_eq!(
            (hash("md5").compute)(b"abc"),
            [
                0x90, 0x01, 0x50, 0x98, 0x3c, 0xd2, 0x4f, 0xb0, 0xd6, 0x96, 0x3f, 0x7d, 0x28, 0xe1,
                0x7f, 0x72
            ]
        );
    }

    #[test]
    fn table_stride() {
        assert_eq!(SignatureTable::stride(hash("md5")), 28);
    }

    #[test]
    fn table_round_trip() {
        let md5 = hash("md5");
        let mut table = SignatureTable::encode(
            md5,
            &[
                Entry {
                    address: 0x1000,
                    size: 0x400,
                    check: 0,
                    expected: vec![],
                    full_sector: true,
                    expected_erased: false,
                },
                Entry {
                    address: 0x2000,
                    size: 0x800,
                    check: 0,
                    expected: vec![],
                    full_sector: true,
                    expected_erased: false,
                },
            ],
        );
        assert_eq!(&table[0..8], &[0x00, 0x10, 0, 0, 0x00, 0x04, 0, 0]);

        // The algorithm signs the first entry, and finds the second one empty.
        table[8..12].copy_from_slice(&STATUS_SIGNED.to_le_bytes());
        table[12..28].copy_from_slice(&[7; 16]);
        table[36..40].copy_from_slice(&STATUS_EMPTY.to_le_bytes());

        assert_eq!(
            SignatureTable::decode(md5, &table).collect::<Vec<_>>(),
            vec![(STATUS_SIGNED, vec![7; 16]), (STATUS_EMPTY, vec![0; 16])]
        );
    }

    fn entry(expected: &[u8], full_sector: bool, expected_erased: bool) -> Entry {
        Entry {
            check: 0,
            address: 0,
            size: 0,
            expected: expected.to_vec(),
            full_sector,
            expected_erased,
        }
    }

    #[test]
    fn outcomes() {
        let signed = |signature: &[u8]| (STATUS_SIGNED, signature.to_vec());
        let empty = (STATUS_EMPTY, vec![]);

        let full = entry(&[1], true, false);
        assert_eq!(
            check_outcome([(&full, &signed(&[1]))].into_iter()),
            Some(CompareOutcome::Match)
        );
        assert_eq!(
            check_outcome([(&full, &signed(&[2]))].into_iter()),
            Some(CompareOutcome::Mismatch)
        );
        assert_eq!(
            check_outcome([(&full, &empty)].into_iter()),
            Some(CompareOutcome::Empty)
        );

        let erased = entry(&[1], true, true);
        assert_eq!(
            check_outcome([(&erased, &empty)].into_iter()),
            Some(CompareOutcome::Match)
        );

        // An empty range in a partial check says nothing about the rest of the sector.
        let partial = entry(&[1], false, false);
        assert_eq!(
            check_outcome([(&partial, &empty)].into_iter()),
            Some(CompareOutcome::Mismatch)
        );

        // Image data that is all erased matches an empty range.
        let partial_erased = entry(&[1], false, true);
        assert_eq!(
            check_outcome([(&partial_erased, &empty)].into_iter()),
            Some(CompareOutcome::Match)
        );
    }

    #[test]
    fn entries_of_erased_image_data_expect_erased_flash() {
        let signatures = AlgorithmSignatures {
            hash: hash("md5"),
            function: "Signatures_md5".into(),
        };
        let erased_data = [0xFF; 0x400];
        let check = SectorCheck::new(
            0x0..0x1000,
            vec![(0x0, &erased_data[..]), (0x800, &[1, 2])],
            Outside::Any,
        );

        let entries = signatures.entries(&[check], 0xFF);

        let erased = entries
            .iter()
            .map(|entry| entry.expected_erased)
            .collect::<Vec<_>>();
        assert_eq!(erased, vec![true, false]);
    }
}
