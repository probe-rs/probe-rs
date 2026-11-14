CC27xx: downloads use a RAM flash algorithm when the AHB-AP is open and the SCFG doesn't change, and SACI otherwise. `probe-rs erase` keeps the CCFG, the SCFG and the HSM firmware.
