An SWD probe now implements `SwdProbe` or `BitbangSwd` in place of `RawDapAccess`, `RawSwdIo`, and `DapProbe`, so a probe driver no longer handles ARM debug concerns.
SWD transfers now run as one batch, so an SWD target needs fewer round trips to read memory.
