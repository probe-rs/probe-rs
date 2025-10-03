Added hints on Linux that warn the user if their setup might be incorrect.

The hints cover the following cases:

- The user has no udev rule file containing the string `probe-rs`.
- The `plugdev` group is missing 
- The `plugdev` group is not a system group (ie. the group has id >= 1000) and
  uses a recent systemd version (ie >= 258).
  See https://github.com/probe-rs/probe-rs/issues/3566 for more info.
- The user does not belong to the `plugdev` group.

You can disable the hints if by setting the `PROBE_RS_DISABLE_SETUP_HINTS` variable.
