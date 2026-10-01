Removed `RawDapAccess`, `RawSwdIo` and `DapProbe`. SWD probes implement `SwdProbe` or `BitbangSwd` instead, and run SWD transfers as one batch.
