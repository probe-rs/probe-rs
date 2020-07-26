use anyhow::Result;
use probe_rs::{
    architecture::arm::{
        swo::{Decoder, ExceptionAction, ExceptionType, TimeStamp, TracePacket, TracePackets},
        SwoConfig,
    },
    Probe,
};
use serde::Serialize;
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
#[structopt(name = "example", about = "An example of StructOpt usage.")]
struct Opt {
    /// Output file, stdout if not present
    #[structopt(parse(from_os_str))]
    output_file: PathBuf,
    #[structopt(short = "d", long = "duration")]
    duration: Option<u64>,
    #[structopt(short = "b", long = "baud")]
    baud: Option<u32>,
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
        tid: usize,
        ts: f64,
        name: String,
    },
    #[serde(rename = "E")]
    DurationEventEnd {
        pid: usize,
        tid: usize,
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

    let opt = Opt::from_args();

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })
    .expect("Error setting Ctrl-C handler");

    let duration = std::time::Duration::from_millis(opt.duration.unwrap_or(u64::MAX));
    let t = std::time::Instant::now();

    // Get a list of all available debug probes.
    let probes = Probe::list_all();

    // Use the first probe found.
    let probe = probes[0].open()?;

    // Attach to a chip.
    let mut session = probe.attach("stm32f407")?;

    // Create a new SwoConfig with a system clock frequency of 16MHz
    let baud = opt.baud.unwrap_or(1_000_000);
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
                            ExceptionType::ExternalInterrupt(n) => match action {
                                ExceptionAction::Entered => {
                                    trace_events.push(TraceEvent::DurationEventBegin {
                                        pid: 1,
                                        tid: n,
                                        ts: timestamp * 1000.0,
                                        name: "KEK".to_string(),
                                    });
                                }
                                ExceptionAction::Exited => {
                                    trace_events.push(TraceEvent::DurationEventEnd {
                                        pid: 1,
                                        tid: n,
                                        ts: timestamp * 1000.0,
                                        name: "KEK".to_string(),
                                    });
                                }
                                ExceptionAction::Returned => continue,
                            },
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
            .open(opt.output_file)?,
    );

    let trace = Trace {
        traceEvents: trace_events,
    };

    writer.write(serde_json::to_string_pretty(&trace)?.as_bytes())?;
    writer.flush()?;

    Ok(())
}
