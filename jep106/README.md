# jep106

This crate provides a means to retrieve the JEDEC manufacturer string for a corresponding JEP106 ID Code.

All the codes can be found on the page of the JEDEC organization but are presented in the riddiculous form of a PDF. This crate parses the PDF and exposes an interface to poll the JEDEC manufacturer string of a JEP106 ID code.

## Status

The crate provides the JEP106AY Revision of the codes list which was published in February 2019.

## Usage

See [docs](https://docs.rs/jep106/).

## License

Licensed under either of

 * Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
   http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or
   http://opensource.org/licenses/MIT) at your option.

### Contributing

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.