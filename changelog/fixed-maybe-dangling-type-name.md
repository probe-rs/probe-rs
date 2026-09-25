The debugger now shows the type that a `ManuallyDrop` or a `MaybeDangling` wraps, and not the wrapper. A `heapless::Vec` of `u32` now reads as `[u32; 4]`, and not as `[MaybeDangling<u32>; 4]`.
