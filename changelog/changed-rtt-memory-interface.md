RTT functions take any `MemoryInterface` instead of a `Core`. Attaching also takes an `RttTarget` with the pointer width and memory regions, which `RttTarget::from(&core)` builds.
