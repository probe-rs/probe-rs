The public `RawDapAccess` and `DapProbe` traits were removed, so callers use layer-0 `SwdProbe`, `BitbangSwd`, and `JtagChain` instead of a combined raw DAP path.
`ArmCommunicationInterface::create` was removed, so new code must use `create_swd` or `create_jtag`.
