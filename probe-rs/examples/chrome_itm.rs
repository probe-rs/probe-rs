use anyhow::Result;
use probe_rs::{
    architecture::arm::{
        swo::{Decoder, ExceptionAction, ExceptionType, TracePacket},
        SwoConfig,
    },
    Probe,
};
use serde::Serialize;
use serde_json::json;
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
}

#[derive(Serialize)]
enum EventType {
    X,
    B,
    E,
}

#[derive(Serialize)]
struct TraceEvent {
    pid: usize,
    tid: usize,
    ts: f64,
    dur: f64,
    ph: EventType,
    name: String,
}

#[derive(Serialize)]
struct Trace {
    traceEvents: Vec<TraceEvent>,
    meta_user: String,
    meta_cpu_count: String,
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
    let cfg = SwoConfig::new(16_000_000)
        .set_baud(1_000_000)
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
        while let Some(packet) = decoder.pull() {
            match packet {
                TracePacket::TimeStamp { tc, ts } => {
                    println!("Timestamp packet: tc={} ts={}", tc, ts);
                    let mut time_delta: f64 = ts as f64;
                    // Divide by core clock frequency to go from ticks to seconds.
                    // TODO: Somehow there is a factor of two configured. Check the prescaler!
                    time_delta /= 16_000_000.0 / 2.0;
                    timestamp += time_delta;
                }
                TracePacket::ExceptionTrace { exception, action } => {
                    println!("{:?} {:?}", action, exception);
                    match exception {
                        ExceptionType::ExternalInterrupt(n) => trace_events.push(TraceEvent {
                            pid: 1,
                            tid: n,
                            ts: timestamp * 1000.0,
                            dur: 10.0,
                            ph: match action {
                                ExceptionAction::Entered => EventType::B,
                                ExceptionAction::Exited => EventType::E,
                                ExceptionAction::Returned => EventType::B,
                            },
                            name: "KEK".to_string(),
                        }),
                        _ => (),
                    }
                }
                // TracePacket::ItmData { id, payload } => {
                //     // First decode the string data from the stimuli.
                //     stimuli[id].push_str(&String::from_utf8_lossy(&payload));
                //     // Then collect all the lines we have gotten so far.
                //     let data = stimuli[id].clone();
                //     let mut lines: Vec<_> = data.lines().collect();

                //     // If there is at least one char in the total of all received chars, look at the last one.
                //     let last_char = stimuli[id].chars().last();
                //     if let Some(last_char) = last_char {
                //         // If the last one is not a newline (this is always the last one for Windows (\r\n) as well as Linux (\n)),
                //         // we keep the last line as it was not fully received yet.
                //         if last_char != '\n' {
                //             // Get the last line and keep it if there is even one.
                //             if let Some(last_line) = lines.pop() {
                //                 stimuli[id] = last_line.to_string();
                //             }
                //         } else {
                //             stimuli[id] = String::new();
                //         }
                //     }

                //     // Finally print all due lines!
                //     for line in lines {
                //         println!("{}> {}", id, line);
                //     }
                // }
                _ => {
                    log::warn!("Trace packet: {:?}", packet);
                }
            }
            log::debug!("{}", timestamp);
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
        meta_user: "probe-rs".to_string(),
        meta_cpu_count: "1".to_string(),
    };

    writer.write(serde_json::to_string(&trace)?.as_bytes())?;
    writer.flush()?;

    Ok(())
}
