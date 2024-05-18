If no `RUST_LOG` environment variable is set, the probe-rs DAP server will now use a default logging configuration of `probe_rs=warn`.  The logs are sent to the DAP client's Debug Console, and a typical session should produce logs similar to those below.

```log
probe-rs-debug: Log output for "probe_rs=warn" will be written to the Debug Console.
probe-rs-debug: Starting probe-rs as a DAP Protocol server
probe-rs-debug: Listening for requests on port 63858
probe-rs-debug: Starting debug session from   :127.0.0.1:63860
FLASHING: Starting write of "/Users/home/project-dir/target/thumbv7em-none-eabihf/debug/project_binary" to device memory
FLASHING: Completed write of "/Users/home/project-dir/target/thumbv7em-none-eabihf/debug/project_binary" to device memory
 WARN probe_rs::util::common_options:239: Unable to use specified speed of 48000 kHz, actual speed used is 24000 kHz
probe-rs-debug: Opened a new RTT Terminal window named: MyRttChannelName
probe-rs-debug: RTT Window opened, and ready to receive RTT data on channel 0

...snipped output...

probe-rs-debug: Closing probe-rs debug session
```