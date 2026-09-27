The GDB server now resumes the target when GDB detaches, and closes the debug session on SIGTERM or Ctrl-C (without `--gdb`) instead of being killed.
