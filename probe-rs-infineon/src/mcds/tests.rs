//! Tests of the MCDS recorder against a model of the TC27x ED bus.

use std::collections::HashMap;

use probe_rs::architecture::tricore::bus::{BusLimits, TransferKind, TransferStatus};

use super::*;

/// The configuration the Infineon MCDS configurator made for a TC27x ED after reset and halt.
const HAR_CONFIG: &[u8] = include_bytes!("testdata/tc27xed_har.mcds");
const JTAG_ID: u32 = 0x401D_A083;
const BLOCKED: u8 = 0x8D;

#[derive(Debug, Default)]
struct Bus {
    words: HashMap<(AddressMap, u32), u32>,
    /// The writes in order.
    writes: Vec<(AddressMap, u32, u32)>,
    /// No EMEM: not an emulation device.
    no_emem: bool,
    /// The EEC map is blocked, as on the DAS JTAG adapter of a ShieldBuddy.
    eec_blocked: bool,
    resets: usize,
    port_reads: u8,
}

impl Bus {
    fn set(&mut self, map: AddressMap, address: u32, value: u32) {
        self.words.insert((map, address), value);
    }

    fn read(&mut self, map: AddressMap, address: u32, len: usize) -> Option<Vec<u8>> {
        if map == AddressMap::Eec && self.eec_blocked || map == AddressMap::Server {
            return None;
        }
        if map == AddressMap::Target && address == EMEM_PROBE && self.no_emem {
            return None;
        }
        if map == AddressMap::Eec && address == TC2XX_ED_CTRL + OFF_TRACE_PORT {
            // Every read of the trace port delivers the next block.
            self.port_reads += 1;
            return Some(vec![self.port_reads; len]);
        }
        let word = self.words.get(&(map, address)).copied().unwrap_or(0);
        Some(
            word.to_le_bytes()
                .iter()
                .copied()
                .cycle()
                .take(len)
                .collect(),
        )
    }
}

impl TriCoreBusAccess for Bus {
    fn execute(&mut self, transfers: &mut [BusTransfer]) -> Result<(), TriCoreError> {
        for transfer in transfers {
            let ok = match transfer.kind {
                TransferKind::Read { len } => {
                    match self.read(transfer.map, transfer.address, len) {
                        Some(data) => {
                            transfer.data = data;
                            true
                        }
                        None => false,
                    }
                }
                TransferKind::Write => {
                    if transfer.map == AddressMap::Eec && self.eec_blocked {
                        false
                    } else {
                        let value = u32::from_le_bytes(transfer.data[..4].try_into().unwrap());
                        self.writes.push((transfer.map, transfer.address, value));
                        if !(transfer.address == 0xFA00_6000) {
                            self.set(transfer.map, transfer.address, value);
                        }
                        true
                    }
                }
            };
            if !ok {
                transfer.status = Some(TransferStatus::Failed(BLOCKED));
                return Ok(());
            }
            transfer.status = Some(TransferStatus::Ok);
        }
        Ok(())
    }

    fn limits(&self) -> BusLimits {
        BusLimits::default()
    }

    fn reset(&mut self, _kind: ResetKind) -> Result<(), TriCoreError> {
        self.resets += 1;
        Ok(())
    }

    fn jtag_id(&mut self) -> Result<u32, TriCoreError> {
        Ok(JTAG_ID)
    }
}

fn ed_bus() -> Bus {
    let mut bus = Bus::default();
    // 96 * 32 B of trace data recorded.
    bus.set(AddressMap::Eec, TC2XX_ED_CTRL + OFF_TRACE_SIZE, 96);
    bus
}

fn options(reset: bool) -> RecordOptions {
    RecordOptions {
        reset,
        duration: Duration::ZERO,
        cleanup: None,
    }
}

fn timestamp() -> Timestamp {
    Timestamp {
        year: 2026,
        month: 10,
        day: 23,
        hour: 9,
        minute: 5,
        second: 7,
    }
}

#[test]
fn an_mcds_file_gives_its_configuration() {
    let config = McdsConfig::parse(HAR_CONFIG).unwrap();
    assert_eq!(config.mcds_device.as_deref(), Some("TC2XX_ED"));
    assert_eq!(config.device_type, Some(JTAG_ID));
    assert_eq!(config.writes.len(), 43);
    assert_eq!(config.writes[0], (0xF000_0478, 0xA1));
    assert_eq!(config.writes[42], (0xF000_0490, 0));

    // The trace data after START_OF_TDR is ignored, even if it is not text.
    let mut file = HAR_CONFIG.to_vec();
    file.extend(b"START_OF_TDR\n\xFF\xFE\x00F0000000 1\n");
    assert_eq!(McdsConfig::parse(&file).unwrap(), config);
}

#[test]
fn a_plain_list_is_a_configuration() {
    let config = McdsConfig::parse(b"# comment\n0xF90E6000 100\n\n9F000000 FFFFFFFF\n").unwrap();
    assert_eq!(
        config.writes,
        [(0xF90E_6000, 0x100), (0x9F00_0000, 0xFFFF_FFFF)]
    );
    assert!(matches!(
        McdsConfig::parse(b"F90E6000\n"),
        Err(McdsError::ConfigSyntax { line: 1, .. })
    ));
    assert!(matches!(
        McdsConfig::parse(b"\n# nothing\n"),
        Err(McdsError::EmptyConfig)
    ));
}

#[test]
fn device_view_addresses_go_to_the_eec_map() {
    assert_eq!(wire_address(0xF90E_6034), (AddressMap::Eec, 0xF00E_6034));
    assert_eq!(wire_address(0x9F00_FFE0), (AddressMap::Eec, 0xAF00_FFE0));
    assert_eq!(wire_address(0xF000_0490), (AddressMap::Target, 0xF000_0490));
}

#[test]
fn a_reset_recording_writes_the_configuration_and_reads_the_trace() {
    let config = McdsConfig::parse(HAR_CONFIG).unwrap();
    let mut bus = ed_bus();
    let mut updates = 0;
    let recording = record(&mut bus, &config, &options(true), timestamp, |_| {
        updates += 1
    })
    .unwrap();
    assert_eq!(bus.resets, 1);
    assert!(updates >= 1);

    // The device registers first, then the MCDS registers, then the CPU0 start.
    let soc = config.writes[..42]
        .iter()
        .filter(|(a, _)| wire_address(*a).0 == AddressMap::Target)
        .count();
    let config_writes = bus
        .writes
        .iter()
        .skip_while(|(_, a, _)| *a == 0xFA00_6000)
        .take(43)
        .collect::<Vec<_>>();
    assert!(
        config_writes[..soc]
            .iter()
            .all(|w| w.0 == AddressMap::Target)
    );
    assert!(
        config_writes[soc..42]
            .iter()
            .all(|w| w.0 == AddressMap::Eec)
    );
    assert_eq!(*config_writes[0], (AddressMap::Target, 0xF000_0478, 0xA1));
    assert_eq!(*config_writes[soc], (AddressMap::Eec, 0xF00E_6000, 0x100));
    assert_eq!(*config_writes[42], (AddressMap::Target, 0xF000_0490, 0));
    // Then the stop.
    assert!(
        bus.writes
            .contains(&(AddressMap::Eec, TC2XX_ED_CTRL + OFF_TRACE_CTRL, TRACE_STOP))
    );

    // 96 * 32 B = 3 KiB, rounded up to 4 KiB, plus 1/32 overhead: 5 blocks.
    assert_eq!(recording.trace.len(), 5 * 1024);
    assert_eq!(recording.trace[0], 1);
    assert_eq!(recording.trace[4 * 1024], 5);
    assert_eq!(recording.device_name, "TC27xED");
    assert_eq!(recording.frequency_hz, None);

    let mut file = Vec::new();
    recording.write_mcds(&mut file, "probe-rs").unwrap();
    let header_end = find(&file, b"START_OF_TDR\n").unwrap() + 13;
    let header = std::str::from_utf8(&file[..header_end]).unwrap();
    assert!(header.starts_with(
        "McdsDevice=TC2XX_ED\nMcdsDeviceUsed=0x401DA083  (TC27xED)\nMcdsToolUsed=\"probe-rs\"\n\
         RecordingDate=2026-10-23\nRecordingTime=09:05:07\n\nSTART_OF_MCDS_CONFIG\n\
         F0000478 000000A1\n"
    ));
    assert!(header.ends_with("F0000490 00000000\nSTART_OF_TDR\n"));
    assert_eq!(&file[header_end..], &recording.trace[..]);
    // The file reads back as the same configuration.
    assert_eq!(McdsConfig::parse(&file).unwrap(), config);
}

#[test]
fn a_blocked_eec_map_is_reported_before_anything_is_written() {
    let config = McdsConfig::parse(HAR_CONFIG).unwrap();
    let mut bus = ed_bus();
    bus.eec_blocked = true;
    let error = record(&mut bus, &config, &options(false), timestamp, |_| {}).unwrap_err();
    assert!(matches!(error, McdsError::McdsNotAccessible(_)), "{error}");
    assert!(bus.writes.is_empty());
    assert_eq!(bus.resets, 0);
}

#[test]
fn a_busy_mcds_is_not_configured() {
    let config = McdsConfig::parse(HAR_CONFIG).unwrap();
    let mut bus = ed_bus();
    bus.set(AddressMap::Eec, TC2XX_ED_CTRL + OFF_OCS, 0x10);
    let error = record(&mut bus, &config, &options(false), timestamp, |_| {}).unwrap_err();
    assert!(matches!(error, McdsError::McdsBusy(0x10)), "{error}");
    assert!(bus.writes.is_empty());
}

#[test]
fn a_device_without_emem_is_refused() {
    let config = McdsConfig::parse(HAR_CONFIG).unwrap();
    let mut bus = ed_bus();
    bus.no_emem = true;
    let error = record(&mut bus, &config, &options(true), timestamp, |_| {}).unwrap_err();
    assert!(matches!(error, McdsError::NotEmulationDevice), "{error}");
    assert!(bus.writes.is_empty());
    assert_eq!(bus.resets, 0);
}

#[test]
fn configurations_are_checked_before_the_device_is_touched() {
    let mut bus = ed_bus();
    // A reset recording must end with the CPU0 start.
    let mut config = McdsConfig::parse(HAR_CONFIG).unwrap();
    config.writes.push((0xF90E_8210, 0x4400));
    let error = record(&mut bus, &config, &options(true), timestamp, |_| {}).unwrap_err();
    assert!(
        matches!(error, McdsError::HarWithoutCpuStart(0xF90E_8210)),
        "{error}"
    );

    // A configuration for another device.
    let mut config = McdsConfig::parse(HAR_CONFIG).unwrap();
    config.device_type = Some(0x1020_7083);
    let error = record(&mut bus, &config, &options(true), timestamp, |_| {}).unwrap_err();
    assert!(matches!(error, McdsError::WrongDevice { .. }), "{error}");

    // No EMEM setup.
    let config = McdsConfig::parse(b"F0000490 0\n").unwrap();
    let error = record(&mut bus, &config, &options(true), timestamp, |_| {}).unwrap_err();
    assert!(matches!(error, McdsError::NoEmemConfig), "{error}");

    assert!(bus.writes.is_empty());
    assert_eq!(bus.resets, 0);
}

#[test]
fn the_cleanup_is_written_after_the_trace() {
    let config = McdsConfig::parse(HAR_CONFIG).unwrap();
    let mut bus = ed_bus();
    let options = RecordOptions {
        cleanup: Some(McdsConfig::parse(b"F90E6000 0\nF90E8210 0\n").unwrap()),
        ..options(false)
    };
    record(&mut bus, &config, &options, timestamp, |_| {}).unwrap();
    assert_eq!(
        bus.writes[bus.writes.len() - 2..],
        [
            (AddressMap::Eec, 0xF00E_6000, 0),
            (AddressMap::Eec, 0xF00E_8210, 0)
        ]
    );
}
