A RISC-V hart reset no longer asserts `hartreset` again while it waits for the halt, so a hart that needs more than one debug module access to reset and halt no longer times out.
