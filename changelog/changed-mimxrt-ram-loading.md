MIMXRT10xx and 11xx debug sequences consult a `PROBE_RS_MIMXRT_FLEXRAM_LAYOUT`
environment variable. If set and valid, the sequence applies the layout during
RAM loading. See the `FlexRamLayout` documentation for the string specification.
Library users can construct the debug sequences with(out) the layout.
