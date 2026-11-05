CMSIS-DAP probes now keep `DAP_Transfer` packets in flight as well as `DAP_TransferBlock` packets, up to the packet count of the probe. A request that writes still waits for the replies before it.
