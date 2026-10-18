Added support for memory access ports.

- Memory access ports can be declared in the target YAML file. Some systems have separate memory
  access ports which access the system memory bus directly.
- New `MemoryAccessPort` structure which implements the `MemoryInterface` trait. It is built from
  the YAML file and returned by `Session::memory_access_port(index)`.
