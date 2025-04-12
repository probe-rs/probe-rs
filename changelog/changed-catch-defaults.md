- Changed the default reset and hardfault catch behavior from no-catch to catch for `probe-rs run` and `probe-rs attach`.
  The --catch-reset and --catch-hardfault flags still exist but now have no effect.
- Added new --no-catch-reset and --no-catch-hardfault flags to turn off `catch-reset` and `catch-hardfault`, respectively