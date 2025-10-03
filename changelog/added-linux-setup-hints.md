Added hints on Linux that warn the user if their setup might be incorrect.

The hints cover the following cases:

- The user has no udev rule file containing the string `probe-rs`.
- The user uses systemd >= 258 and the `plugdev` group is missing or has an id <= 1000.
  See https://github.com/probe-rs/probe-rs/issues/3566 for more info.
- The user does not belong to the `plugdev` group.
