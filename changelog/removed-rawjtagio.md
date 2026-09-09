A JTAG probe now implements `BitbangJtag` or `JtagProbe` in place of `RawJtagIo` and `AutoImplementJtagAccess`, so a probe no longer tracks the state of the TAP.
