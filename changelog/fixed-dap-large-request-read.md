The DAP server reads a request until it is complete. Before, it read one buffer per poll of the target, so a request that carries a program binary (remote server mode) took minutes to arrive.
