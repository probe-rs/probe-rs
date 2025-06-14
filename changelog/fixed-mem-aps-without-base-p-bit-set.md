Fixed an issue where a MEM-AP whose BASE register did not have Present bit set (no ROM tables) would cause probe-rs to issue invalid memory accesses. This was discovered on an STM32H743.
