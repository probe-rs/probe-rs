# Design

## Traits

### Memory interfacing

To talk to a specific target memory, one should implement the memory trait.
Each method will accept a DAP interface as the first argument to be able to communicate with the target.

```rust

    trait Memory<I: DAPInterface> {
        fn read_memory_32(interface: &mut I, address: u32) -> Result<u32, Error>;
        fn read_memory_16(interface: &mut I, address: u32) -> Result<u16, Error>;
        fn read_memory_8(interface: &mut I, address: u32) -> Result<u8, Error>;
        fn write_memory_32(interface: &mut I, address: u32, value: u32) -> Result<(), Error>;
        fn write_memory_16(interface: &mut I, address: u32, value: u16) -> Result<(), Error>;
        fn write_memory_8(interface: &mut I, address: u32, value: u8) -> Result<(), Error>;
    }

```
### Probe interfacing

Probes should implement the following interfaces to talk to the different ports available.
We use TypeStates to ensure everything with the probe is in a proper state to perform different operations.

```rust

    struct DAP;
    struct DP;
    struct AP;

    struct Connected<PROTOCOL, MODE> {
        _marker1: PhantomData<MODE>,
        _marker2: PhantomData<MODE>
    };
    struct Disconnected;

    trait DAPInterface {
        fn read_dap_register(&mut self) -> Result<(), Error>;
        fn write_dap_register(&mut self) -> Result<(), Error>;
    }

    trait DPInterface {
        fn read_dp_register(&mut self) -> Result<(), Error>;
        fn write_dp_register(&mut self) -> Result<(), Error>;
    }

    trait APInterface {
        fn read_ap_register(&mut self) -> Result<(), Error>;
        fn write_ap_register(&mut self) -> Result<(), Error>;
    }

    trait ConnectedProbe<PROBE> {
        fn disconnect(&mut self) -> Result<PROBE, Error>;
    }

    trait ConnectedProbe<PROBE> {
        fn disconnect(&mut self) -> Result<PROBE, Error>;
    }

    trait DisconnectedProbe<PROBE> {
        fn connect(&mut self) -> Result<PROBE, Error>;
    }

```

An example impl for an ST-Link looks like this:

```rust

    struct STLink<STATE> {
        _marker: PhantomData<STATE>
    }

    impl<MODE> ConnectedProbe<Disconnected> for STLink<Connected<MODE>> {
        fn disconnect(&mut self) -> Result<Disconnected, Error> {
            Ok(())
        }
    }

    impl<MODE> DisconnectedProbe<Connected<MODE>> for STLink<Disconnected> {
        fn connect(&mut self) -> Result<Connected<MODE>, Error> {
            Ok(())
        }
    }

    impl DAPInterface for STLink<Connected<DAP>> {
        fn read_dap_register(&mut self) -> Result<(), Error> {
            Ok(())
        }

        fn write_dap_register(&mut self) -> Result<(), Error> {
            Ok(())
        }
    }

    impl DPInterface for STLink<Connected<DP>> {
        fn read_dp_register(&mut self) -> Result<(), Error> {
            Ok(())
        }
        
        fn write_dp_register(&mut self) -> Result<(), Error> {
            Ok(())
        }
    }

    impl APInterface for STLink<Connected<AP>> {
        fn read_ap_register(&mut self) -> Result<(), Error> {
            Ok(())
        }
        
        fn write_ap_register(&mut self) -> Result<(), Error> {
            Ok(())
        }
    }

```