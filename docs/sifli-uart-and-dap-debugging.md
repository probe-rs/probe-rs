# SiFli UART and DAP Debugging Notes

This document explains the recent changes around the SiFli UART probe
implementation and the DAP server output path.

It focuses on two problems:

1. `sifliuart` debug logs were too noisy to be useful when large memory
   transfers were in flight.
2. the DAP server could fail with `os error 35` while sending large responses,
   especially `variables` responses, because the socket write path did not
   tolerate temporary backpressure.

## What Changed

### 1. SiFli UART logging is now layered

The SiFli UART probe now separates control-plane logs from data-plane logs.

- `debug` logs now show command and response summaries only.
- `trace` logs show raw TX/RX frame previews.
- large `MEMRead` and `MEMWrite` payloads are no longer expanded into the log.

Examples:

- `SiFli UART command: MEMWrite { addr: 0x20000000, words: 64, bytes: 256 }`
- `SiFli UART response: MEMRead { bytes: 128 }`
- `TX SiFli UART frame`
- `RX SiFli UART frame`

This makes `debug` useful for flow analysis, while `trace` remains available
when raw frame inspection is required.

### 2. The SiFli UART parser is stricter and safer

The transport parser now treats a packet as a debug frame only if the frame
header matches the expected SiFli debug frame type.

It also reinjects invalid frame candidates back into the UART console stream
instead of treating them as protocol failures.

This change was made because regular target console bytes can occasionally
contain the same prefix as a debug frame. Before the fix, that could cause:

- false protocol responses,
- dropped console bytes,
- misleading timeout or parse errors.

### 3. Panic paths were removed from the SiFli UART probe

Several failure paths that previously used `unwrap()` or `todo!()` now return
normal errors or log a debug message.

This makes reset and reconnect scenarios easier to diagnose and avoids
crashing the whole debug session because of a transient UART failure.

### 4. DAP response writes now retry on temporary backpressure

The DAP server uses a non-blocking TCP socket. That is fine for reads, but the
write path previously used blocking-style `write_all()` and `flush()` calls
without any retry handling.

When the IDE temporarily stopped draining the socket quickly enough, large
responses such as `variables` could fail with:

`Resource temporarily unavailable (os error 35)`

The write path now retries `WouldBlock` and `Interrupted` errors for a short
period before reporting a real timeout.

This significantly reduces failures when expanding large variable trees.

### 5. DAP response logging is summarized

The DAP server no longer prints entire response bodies in ordinary debug logs.
Instead, it logs a short summary such as:

- `body=variables[143]`
- `body=error(id=0)`
- `body=object{foo,bar,+2}`

This change was necessary because the old logging behavior could flood the
debug console with huge `variables` payloads and make the actual failure
signal hard to see.

## Why These Changes Were Needed

### Why summarize SiFli UART traffic?

When debugging memory access bugs, the useful information is usually:

- which command was sent,
- which address and size were used,
- whether a response arrived,
- whether the transaction timed out or was rejected.

Dumping every byte of every `MEMWrite` at `debug` level made it difficult to
follow the sequence of operations.

The new behavior preserves the high-value information at `debug`, while still
allowing deeper inspection at `trace`.

### Why validate the frame type?

The SiFli UART transport multiplexes protocol traffic and plain console data on
the same serial channel. That means the parser must be conservative.

If the parser is too eager, ordinary console output can be mistaken for a
debug response. Once that happens, the tool can report the wrong failure and
also lose the original console bytes.

The stricter frame validation avoids that.

### Why retry DAP writes?

`os error 35` on macOS means the socket would block. For a local TCP-based DAP
session, that usually indicates transient backpressure, not a broken session.

Large `variables` responses are the most common trigger because they can be far
larger than ordinary request and response traffic.

Retrying these writes is the correct behavior for a non-blocking socket.

## How To Use The New Behavior

### SiFli UART logging from the CLI

Use `debug` when you want to understand the command flow:

```bash
RUST_LOG=probe_rs::probe::sifliuart=debug \
probe-rs --log-file /tmp/probe-rs.log info --chip <chip>
```

Use `trace` when you need raw frame previews:

```bash
RUST_LOG=probe_rs::probe::sifliuart=trace \
probe-rs --log-file /tmp/probe-rs.log gdb --chip <chip>
```

Recommendations:

- prefer `debug` for ordinary investigation,
- use `trace` only when you really need frame-level details,
- write `trace` output to a file instead of streaming it to an interactive
  console.

### DAP server logging

The DAP server has two separate knobs:

1. `RUST_LOG`
2. `consoleLogLevel`

`RUST_LOG` controls tracing output produced by `probe-rs` itself.

`consoleLogLevel` controls how much DAP request and response activity is echoed
into the IDE debug console.

Valid `consoleLogLevel` values are:

- `console`
- `info`
- `debug`

Recommended settings for day-to-day use:

```json
{
  "type": "probe-rs-debug",
  "request": "attach",
  "consoleLogLevel": "info",
  "logFile": "${workspaceFolder}/probe-rs-dap.log"
}
```

If you need more detail:

```json
{
  "type": "probe-rs-debug",
  "request": "attach",
  "consoleLogLevel": "debug",
  "logFile": "${workspaceFolder}/probe-rs-dap.log"
}
```

Suggested environment settings:

```bash
RUST_LOG=probe_rs=debug
```

If you suspect a protocol-level issue and need more than summaries, raise the
log level and keep it in a file:

```bash
RUST_LOG=probe_rs=trace
```

Do not rely on `TRACE` output in the IDE debug console for routine work. It is
too verbose and can distort timing.

## Expected Results

After these changes:

- expanding large variable trees should be less likely to terminate the DAP
  session with `os error 35`,
- `debug` logs should remain readable while using the SiFli UART probe,
- `trace` remains available for packet-level inspection,
- ordinary UART console bytes should no longer be misclassified as debug
  frames as easily as before.

## Scope of the Change

The implementation changes are in:

- `probe-rs/src/probe/sifliuart/mod.rs`
- `probe-rs/src/probe/sifliuart/transport.rs`
- `probe-rs/src/probe/sifliuart/arm.rs`
- `probe-rs-tools/src/bin/probe-rs/cmd/dap_server/debug_adapter/protocol.rs`

The behavior described in this document is intended for users who:

- debug SiFli targets over the UART probe transport,
- use the `probe-rs` DAP server from an IDE,
- or need to capture detailed logs without drowning in bulk payload dumps.
