BREAKING: Removed the `get_dap_access` and `get_swd_sequence` functions from the `ArmMemoryInterface` trait. Calls to these functions can be replaced by `ArmMemoryInterface::get_arm_debug_interface`
