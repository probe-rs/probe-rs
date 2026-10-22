//! Tests of the MCD probe against fake MCD functions linked into the test binary.

use std::{
    collections::HashMap,
    ffi::{CStr, c_char},
    sync::{Mutex, MutexGuard},
};

use probe_rs::architecture::tricore::bus::{
    AddressMap, BusTransfer, ResetKind, TransferStatus, TriCoreBusAccess, TriCoreError,
};

use super::{ffi::*, *};

const JTAG_ID: u32 = 0x401D_A083;
/// Reads at this address fail.
const BAD_ADDRESS: u64 = 0xA021_C000;

/// What the fake library saw, and its memory.
#[derive(Default)]
struct Fake {
    memory: HashMap<u64, u8>,
    calls: Vec<String>,
    /// The number of transactions of every list.
    lists: Vec<usize>,
    resets: Vec<(u32, u32)>,
    last_error: (u32, String),
    open: bool,
}

static FAKE: Mutex<Option<Fake>> = Mutex::new(None);
/// Serialises the tests, which share the fake library.
static TEST: Mutex<()> = Mutex::new(());

fn fake() -> MutexGuard<'static, Option<Fake>> {
    FAKE.lock().unwrap_or_else(|e| e.into_inner())
}

fn with<R>(f: impl FnOnce(&mut Fake) -> R) -> R {
    f(fake().as_mut().unwrap())
}

fn fail(code: u32, text: &str) -> mcd_return_et {
    with(|s| s.last_error = (code, text.into()));
    3 // MCD_RET_ACT_HANDLE_ERROR
}

unsafe extern "C" fn initialize(
    version: *const mcd_api_version_st,
    info: *mut mcd_impl_version_info_st,
) -> mcd_return_et {
    let version = unsafe { &*version };
    assert_eq!((version.v_api_major, version.v_api_minor), (1, 1));
    let info = unsafe { &mut *info };
    set_c_string(&mut info.vendor, b"fake");
    with(|s| s.calls.push("initialize".into()));
    0
}

unsafe extern "C" fn exit() {
    with(|s| s.calls.push("exit".into()));
}

static mut SERVER: mcd_server_st = mcd_server_st {
    instance: ptr::null_mut(),
    host: ptr::null(),
    config_string: ptr::null(),
};

unsafe extern "C" fn open_server(
    _key: *const c_char,
    config: *const c_char,
    server: *mut *mut mcd_server_st,
) -> mcd_return_et {
    let config = if config.is_null() {
        "<none>".to_string()
    } else {
        unsafe { CStr::from_ptr(config) }
            .to_string_lossy()
            .into_owned()
    };
    with(|s| s.calls.push(format!("open_server {config}")));
    unsafe { *server = &raw mut SERVER };
    0
}

unsafe extern "C" fn close_server(_server: *const mcd_server_st) -> mcd_return_et {
    with(|s| s.calls.push("close_server".into()));
    0
}

unsafe extern "C" fn qry_systems(
    _start: u32,
    count: *mut u32,
    info: *mut mcd_core_con_info_st,
) -> mcd_return_et {
    unsafe {
        set_c_string(&mut (*info).system, b"Real HW");
        *count = 1;
    }
    0
}

unsafe extern "C" fn qry_devices(
    _system: *const mcd_core_con_info_st,
    _start: u32,
    count: *mut u32,
    info: *mut mcd_core_con_info_st,
) -> mcd_return_et {
    unsafe {
        set_c_string(&mut (*info).device, b"TC27x");
        set_c_string(&mut (*info).acc_hw, b"DAS JTAG AURDUINO");
        (*info).device_type = JTAG_ID;
        *count = 1;
    }
    0
}

unsafe extern "C" fn qry_cores(
    device: *const mcd_core_con_info_st,
    _start: u32,
    count: *mut u32,
    info: *mut mcd_core_con_info_st,
) -> mcd_return_et {
    unsafe {
        *info = *device;
        set_c_string(&mut (*info).core, b"Core 0: CPU0 TriCore 1.6E");
        (*info).core_type = 44;
        *count = 1;
    }
    0
}

unsafe extern "C" fn open_core(
    info: *const mcd_core_con_info_st,
    core: *mut *mut mcd_core_st,
) -> mcd_return_et {
    with(|s| {
        s.open = true;
        s.calls.push("open_core".into());
    });
    // Like a library, the fake owns the core object; the tests leak it.
    let info: &'static mcd_core_con_info_st = Box::leak(Box::new(unsafe { *info }));
    let core_obj = Box::leak(Box::new(mcd_core_st {
        instance: ptr::null_mut(),
        core_con_info: info,
    }));
    unsafe { *core = core_obj };
    0
}

unsafe extern "C" fn close_core(_core: *const mcd_core_st) -> mcd_return_et {
    with(|s| {
        s.open = false;
        s.calls.push("close_core".into());
    });
    0
}

unsafe extern "C" fn qry_error_info(_core: *const mcd_core_st, info: *mut mcd_error_info_st) {
    let (code, text) = with(|s| s.last_error.clone());
    unsafe {
        (*info).error_code = code;
        set_c_string(&mut (*info).error_str, text.as_bytes());
    }
}

unsafe extern "C" fn qry_max_payload_size(
    _core: *const mcd_core_st,
    max_payload: *mut u32,
) -> mcd_return_et {
    unsafe { *max_payload = 4096 };
    0
}

unsafe extern "C" fn execute_txlist(
    _core: *const mcd_core_st,
    txlist: *mut mcd_txlist_st,
) -> mcd_return_et {
    let txlist = unsafe { &mut *txlist };
    let txs = unsafe { std::slice::from_raw_parts_mut(txlist.tx, txlist.num_tx as usize) };
    let payload: u32 = txs.iter().map(|tx| tx.num_bytes).sum();
    assert!(txs.len() <= MCD_TX_NUM_MAX && payload <= 4096 || txs.len() == 1);
    with(|s| s.lists.push(txs.len()));
    txlist.num_tx_ok = 0;
    for tx in txs.iter_mut() {
        let data = unsafe { std::slice::from_raw_parts_mut(tx.data, tx.num_bytes as usize) };
        if tx.access_width != 0 {
            assert_eq!(tx.access_width as u32, tx.num_bytes);
        }
        let address = tx.addr.address;
        if tx.addr.mem_space_id != 0 {
            return fail(MCD_ERR_TXLIST_TX, "unknown memory space");
        }
        if tx.access_type == MCD_TX_AT_R {
            if (address..address + data.len() as u64).contains(&BAD_ADDRESS) {
                return fail(
                    MCD_ERR_TXLIST_READ,
                    "Read failure for a transaction of the txlist",
                );
            }
            with(|s| {
                for (i, byte) in data.iter_mut().enumerate() {
                    *byte = *s.memory.get(&(address + i as u64)).unwrap_or(&0);
                }
            });
        } else {
            with(|s| {
                for (i, byte) in data.iter().enumerate() {
                    s.memory.insert(address + i as u64, *byte);
                }
            });
        }
        tx.num_bytes_ok = tx.num_bytes;
        txlist.num_tx_ok += 1;
    }
    0
}

unsafe extern "C" fn qry_rst_classes(
    _core: *const mcd_core_st,
    classes: *mut u32,
) -> mcd_return_et {
    unsafe { *classes = 0x7 };
    0
}

unsafe extern "C" fn rst(_core: *const mcd_core_st, class: u32, halt: mcd_bool_t) -> mcd_return_et {
    // `mcd_bool_t` is `u8` on Linux and macOS.
    #[allow(clippy::useless_conversion)]
    let halt = u32::from(halt);
    with(|s| s.resets.push((class, halt)));
    0
}

const FUNCTIONS: McdFunctions = McdFunctions {
    initialize,
    exit,
    open_server,
    close_server,
    qry_systems,
    qry_devices,
    qry_cores,
    open_core,
    close_core,
    qry_error_info,
    qry_max_payload_size,
    execute_txlist,
    qry_rst_classes,
    rst,
};

/// Sets up a fresh fake library and a probe for it.
fn probe(config: &str) -> (MutexGuard<'static, ()>, McdProbe) {
    let guard = TEST.lock().unwrap_or_else(|e| e.into_inner());
    *fake() = Some(Fake::default());
    let endpoint = McdEndpoint {
        library: "fake.dll".into(),
        config: config.into(),
    };
    (guard, McdProbe::with_functions(endpoint, FUNCTIONS))
}

#[test]
fn endpoints_are_parsed() {
    let none = |_: &str| None;
    assert_eq!(
        McdEndpoint::parse(r"C:\tools\mcd_tas.dll", none).unwrap(),
        McdEndpoint {
            library: r"C:\tools\mcd_tas.dll".into(),
            config: String::new(),
        }
    );
    let endpoint = McdEndpoint::parse(
        r"C:\tools\mcd_tas.dll|McdHostName=box McdServerPort=1",
        none,
    )
    .unwrap();
    assert_eq!(endpoint.config, "McdHostName=box McdServerPort=1");
    assert_eq!(
        McdEndpoint::parse(&endpoint.serial(), none).unwrap(),
        endpoint
    );

    assert!(McdEndpoint::parse("", none).is_err());
    let env = |name: &str| match name {
        LIBRARY_ENV => Some("from_env.dll".to_string()),
        CONFIG_ENV => Some("McdHostName=env".to_string()),
        _ => None,
    };
    assert_eq!(
        McdEndpoint::parse("", env).unwrap(),
        McdEndpoint {
            library: "from_env.dll".into(),
            config: "McdHostName=env".into(),
        }
    );
    // An explicit, even empty, configuration wins over the environment.
    assert_eq!(McdEndpoint::parse("x.dll|", env).unwrap().config, "");
}

#[test]
fn the_factory_only_opens_its_selector() {
    let tas: DebugProbeSelector = "058b:0000".parse().unwrap();
    assert!(matches!(
        McdFactory.open(&tas),
        Err(DebugProbeError::ProbeCouldNotBeCreated(
            ProbeCreationError::NotFound
        ))
    ));
    // The rest of a Windows path survives the selector parser.
    let missing: DebugProbeSelector = r"058b:0001:C:\no\such\mcd.dll".parse().unwrap();
    assert_eq!(
        missing.serial_number.as_deref(),
        Some(r"C:\no\such\mcd.dll")
    );
    let error = McdFactory.open(&missing).unwrap_err().to_string();
    assert!(error.contains("could not be loaded"), "{error}");
}

#[test]
fn attach_opens_the_first_core_and_moves_memory() {
    let (_guard, mut probe) = probe("McdHostName=localhost");
    assert!(probe.try_as_tricore_bus().is_none());
    probe.attach().unwrap();
    assert_eq!(probe.core_name(), "Core 0: CPU0 TriCore 1.6E");
    assert_eq!(probe.access_hardware(), "DAS JTAG AURDUINO");
    assert_eq!(probe.jtag_id().unwrap(), JTAG_ID);
    assert_eq!(
        probe.limits(),
        BusLimits {
            max_transfers: 64,
            max_block_size: 1024
        }
    );

    // An unaligned range larger than one list (4 KiB payload).
    let data = (0..9000).map(|i| (i * 7) as u8).collect::<Vec<_>>();
    probe
        .write_block(AddressMap::Target, 0x7000_0003, &data)
        .unwrap();
    let mut back = vec![0; data.len()];
    probe
        .read_block(AddressMap::Target, 0x7000_0003, &mut back)
        .unwrap();
    assert_eq!(back, data);
    assert!(with(|s| s.lists.len()) > 2);

    probe.write_u32(AddressMap::Target, 0xF881_FD00, 6).unwrap();
    assert_eq!(probe.read_u32(AddressMap::Target, 0xF881_FD00).unwrap(), 6);

    probe.detach().unwrap();
    assert!(probe.try_as_tricore_bus().is_none());
    assert_eq!(
        with(|s| s.calls.clone()),
        [
            "initialize",
            "open_server McdHostName=localhost",
            "open_core",
            "close_core",
            "close_server",
            "exit"
        ]
    );
}

#[test]
fn a_failed_read_stops_the_batch() {
    let (_guard, mut probe) = probe("");
    probe.attach().unwrap();
    assert_eq!(with(|s| s.calls[1].clone()), "open_server <none>");

    let mut transfers = [
        BusTransfer::read(AddressMap::Target, 0xA021_BFF8, 8),
        BusTransfer::read(AddressMap::Target, BAD_ADDRESS as u32, 32),
        BusTransfer::read(AddressMap::Target, 0xA021_C020, 32),
    ];
    probe.execute(&mut transfers).unwrap();
    assert_eq!(transfers[0].status, Some(TransferStatus::Ok));
    assert_eq!(
        transfers[1].status,
        Some(TransferStatus::Failed(STATUS_READ_FAILED))
    );
    assert_eq!(transfers[2].status, None);
    assert!(matches!(
        transfers[1].check(),
        Err(TriCoreError::TransferFailed {
            code: STATUS_READ_FAILED,
            ..
        })
    ));
}

#[test]
fn other_address_maps_are_refused() {
    let (_guard, mut probe) = probe("");
    probe.attach().unwrap();
    let mut transfers = [
        BusTransfer::read(AddressMap::Target, 0x7000_0000, 4),
        BusTransfer::read(AddressMap::Server, 0x5000_0090, 4),
        BusTransfer::read(AddressMap::Target, 0x7000_0000, 4),
    ];
    probe.execute(&mut transfers).unwrap();
    assert_eq!(transfers[0].status, Some(TransferStatus::Ok));
    assert_eq!(
        transfers[1].status,
        Some(TransferStatus::Failed(STATUS_MAP_NOT_AVAILABLE))
    );
    assert_eq!(transfers[2].status, None);
}

#[test]
fn resets_halt_with_the_system_reset_class() {
    let (_guard, mut probe) = probe("");
    probe.target_reset_assert().unwrap();
    probe.attach().unwrap();
    probe.reset(ResetKind::ResetAndHalt).unwrap();
    assert_eq!(with(|s| s.resets.clone()), [(0x2, 1), (0x2, 1)]);
}

#[test]
fn a_dropped_probe_closes_the_library() {
    let (_guard, mut probe) = probe("");
    probe.attach().unwrap();
    drop(probe);
    assert!(!with(|s| s.open));
    assert_eq!(with(|s| s.calls.last().cloned()).as_deref(), Some("exit"));
}
