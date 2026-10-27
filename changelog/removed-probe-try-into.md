Removed `Probe::try_into`. Downcast `Probe::driver_mut` through `std::any::Any` instead.
