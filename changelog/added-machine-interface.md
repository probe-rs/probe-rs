probe-rs now has a machine interface (MI).

The machine interface can be accessed via `probe-rs mi` and is intended to have a set of
machine operable & readable commands & responses.
This will allow for example a CI to use probe-rs in predictable fashion or allows auto-updaters to install the most current version.

A separate crate with interface types called `probe-rs-mi` was added to let users easily parse the output of the MI.