# How to release all the probe-rs crates

Currently the probe-rs project includes the following maintained & to be released crates:  
- probe-rs (main release cycle)
- gdb-server (main release cycle)
- probe-rs-cli (main release cycle)
- probe-rs-cli-util (main release cycle)
- probe-rs-target (released optionally when changes are made)
- probe-rs-rtt (main release cycle)
- rtt-target (separate rtt-target release cycle)
- panic-rtt-target (separate rtt-target release cycle)
- cargo-flash (main release cycle)
- cargo-embed (main release cycle)

TODO:
- Release the target-gen crate

Generally the steps to release, which are required are:
1. Make sure all the versions are bumped, both the one of the crate itself and of its probe-rs dependencies!
2. Make sure all the crates build.
3. Make sure the CHANGELOG.md (if present) in each crate holds all the changes for the next release and has the diff links properly updated.
4. Use `cargo publish` in each crate to release the crate (pwd must be the crate root!).
5. Add a new release on Github under (https://github.com/probe-rs/probe-rs/releases and the respective repos) with the changelog attached (see existing releases)!
6. Notify the members in the Matrix channel about the release.