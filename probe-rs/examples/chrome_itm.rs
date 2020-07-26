use anyhow::Result;
use probe_rs::{
    architecture::arm::{
        swo::{Decoder, ExceptionAction, ExceptionType, TimeStamp, TracePacket, TracePackets},
        SwoConfig,
    },
    Probe,
};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use svd::{Device, Interrupt};
use svd_parser as svd;

#[derive(Deserialize)]
#[allow(unused)]
struct Config {
    output_file: PathBuf,
    duration: Option<u64>,
    baud: Option<u32>,
    isr_mapping: HashMap<usize, String>,
    svd: Option<String>,
}

#[derive(Serialize)]
#[allow(unused)]
enum InstantEventType {
    #[serde(rename = "g")]
    Global,
    #[serde(rename = "p")]
    Process,
    #[serde(rename = "t")]
    Thread,
}

#[derive(Serialize)]
#[serde(tag = "ph")]
#[allow(unused)]
enum TraceEvent {
    #[serde(rename = "B")]
    DurationEventBegin {
        pid: usize,
        tid: String,
        ts: f64,
        name: String,
        args: Option<HashMap<String, String>>,
    },
    #[serde(rename = "E")]
    DurationEventEnd {
        pid: usize,
        tid: String,
        ts: f64,
        name: String,
    },
    #[serde(rename = "X")]
    CompleteEvent {
        pid: usize,
        tid: usize,
        ts: f64,
        dur: f64,
        name: String,
    },
    #[serde(rename = "I")]
    InstantEvent {
        pid: usize,
        tid: String,
        ts: f64,
        name: String,
        s: InstantEventType,
    },
}

#[derive(Serialize)]
#[allow(non_snake_case)]
struct Trace {
    traceEvents: Vec<TraceEvent>,
}

fn main() -> Result<()> {
    pretty_env_logger::init();

    let reader = BufReader::new(OpenOptions::new().read(true).open("trace_config.json")?);
    let config: Config = serde_json::from_reader(reader)?;

    let xml = &mut String::new();
    let svd = config
        .svd
        .map(|f| {
            File::open(f)?.read_to_string(xml)?;
            svd::parse(xml)
        })
        .transpose()?;

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })
    .expect("Error setting Ctrl-C handler");

    let duration = std::time::Duration::from_millis(config.duration.unwrap_or(u64::MAX));
    let t = std::time::Instant::now();

    // Get a list of all available debug probes.
    let probes = Probe::list_all();

    // Use the first probe found.
    let probe = probes[0].open()?;

    // Attach to a chip.
    let mut session = probe.attach("stm32f407")?;

    // Create a new SwoConfig with a system clock frequency of 16MHz
    let baud = config.baud.unwrap_or(1_000_000);
    println!("Using {} baud.", baud);
    let cfg = SwoConfig::new(16_000_000)
        .set_baud(baud)
        .set_continuous_formatting(false);

    session.setup_swv(&cfg)?;

    {
        let component = session.get_arm_component()?;
        let mut core = session.core(0)?;
        let mut dwt = component.dwt(&mut core)?;
        dwt.enable_exception_trace()?;
    }

    let mut timestamp: f64 = 0.0;

    let mut decoder = Decoder::new();

    let mut trace_events = vec![];

    println!("Starting SWO trace ...");

    while t.elapsed() < duration && running.load(Ordering::SeqCst) {
        let bytes = session.read_swo()?;

        decoder.feed(bytes);
        while let Some(TracePackets {
            packets,
            timestamp: TimeStamp { tc, ts },
        }) = decoder.pull()
        {
            log::debug!("Timestamp packet: tc={:?} ts={}", tc, ts);
            let mut time_delta: f64 = ts as f64;
            // Divide by core clock frequency to go from ticks to seconds.
            time_delta /= 16_000_000.0;
            timestamp += time_delta;

            for packet in packets {
                match packet {
                    TracePacket::ExceptionTrace { exception, action } => {
                        println!("{:?} {:?}", action, exception);
                        match exception {
                            ExceptionType::Main => {
                                trace_events.push(TraceEvent::DurationEventBegin {
                                    pid: 1,
                                    tid: "0".to_string(),
                                    ts: timestamp * 1000.0,
                                    name: "Main".to_string(),
                                    args: None,
                                });
                            }
                            ExceptionType::ExternalInterrupt(n) => {
                                let isr = get_isr(&svd, n as u32 - 16);
                                let mut args = HashMap::new();
                                let name = isr
                                    .map(|i| {
                                        i.description.as_ref().map(|d| {
                                            args.insert("description".to_string(), d.clone())
                                        });
                                        i.name.clone()
                                    })
                                    .unwrap_or_else(|| "Unknown ISR".to_string());
                                match action {
                                    ExceptionAction::Entered => {
                                        trace_events.push(TraceEvent::DurationEventBegin {
                                            pid: 1,
                                            tid: format!("{}", n),
                                            ts: timestamp * 1000.0,
                                            name,
                                            args: Some(args),
                                        });
                                        // Interrupt main.
                                        trace_events.push(TraceEvent::DurationEventEnd {
                                            pid: 1,
                                            tid: "0".to_string(),
                                            ts: timestamp * 1000.0,
                                            name: "Main".to_string(),
                                        });
                                    }
                                    ExceptionAction::Exited => {
                                        trace_events.push(TraceEvent::DurationEventEnd {
                                            pid: 1,
                                            tid: format!("{}", n),
                                            ts: timestamp * 1000.0,
                                            name,
                                        });
                                    }
                                    ExceptionAction::Returned => continue,
                                }
                            }
                            _ => (),
                        }
                    }
                    TracePacket::ItmData { id, payload } => {
                        // First decode the string data from the stimuli.
                        let payload = String::from_utf8_lossy(&payload);
                        println!("{:?}", payload);
                        trace_events.push(TraceEvent::InstantEvent {
                            pid: 1,
                            tid: "4".to_string(),
                            ts: timestamp * 1000.0,
                            name: payload.to_string(),
                            s: InstantEventType::Global,
                        })
                    }
                    _ => {
                        log::warn!("Trace packet: {:?}", packet);
                    }
                }
                log::debug!("{}", timestamp);
            }
        }
    }

    let mut writer = BufWriter::new(
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(config.output_file)?,
    );

    let trace = Trace {
        traceEvents: trace_events,
    };

    writer.write(serde_json::to_string_pretty(&trace)?.as_bytes())?;
    writer.flush()?;

    Ok(())
}

fn get_isr(device: &Option<Device>, number: u32) -> Option<&Interrupt> {
    device.as_ref().and_then(|device| {
        for peripheral in &device.peripherals {
            for interrupt in &peripheral.interrupt {
                if interrupt.value == number {
                    return Some(interrupt);
                }
            }
        }
        return None;
    })
}
