Added generic support for memory access ports.

- Memory access port information can now be statically declared inside the target YAML file. Some
  systems have separate memory access ports which access the system memory bus directly.
- Added new `MemoryAccessPort` structure which implements the `MemoryInterface` trait and
  can be retrieved using `Session::memory_access_port(index)`. The structure will be constructed
  based on information from the YAML file.
