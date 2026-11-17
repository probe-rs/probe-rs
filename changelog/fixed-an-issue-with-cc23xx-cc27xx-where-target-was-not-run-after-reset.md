Fixed an issue where the cc23xx/cc27xx was not left in the desired state post reset. `reset_system` now captures wanted state before reset and exits SACI accordingly.
