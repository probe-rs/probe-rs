fixed: reconfigure RTT channels after control block layout changes

When firmware transitions from a bootloader RTT instance to an application RTT instance,
probe-rs now re-applies channel configuration if the discovered RTT channel set changes.
This avoids keeping stale per-channel mode assumptions from the previous control block,
which could break defmt output when both bootloader and app expose RTT/defmt.
