Update the `STM32WB_Series.yaml` targets with `target-gen arm --filter STM32WBxx_DFP`
Removed `stack_size` from all `flash_algorithms` files as the values are sometimes incorrect and are not needed for flashing operations.
