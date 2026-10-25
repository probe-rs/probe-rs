`probe-rs run` and `attach` clear the vector catches they enabled when they exit, so a later reset no longer halts the core at the reset vector.
