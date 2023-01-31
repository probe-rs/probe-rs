# Changelog

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/)
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.16.0]

Released 2023-01-29
### Fixed

- Ensure offset between local time and UTC gets determined as early as possible.

  Determining the local time fails in multi-threaded programs, so it needs to be
  done as early as possible. Otherwise the program will quit with an error saying that the local time could not have been determined.

## [0.15.0]

Released 2023-01-28



## [0.14.2]

Released 2023-01-18

## [0.14.1]

Released 2023-01-14

## [0.14.0]

For changes until 0.14.0 see the main CHANGELOG.md with the probe-rs library.

## [0.13.0]

For changes until 0.14.0 see the main CHANGELOG.md with the probe-rs library.

[unreleased]: https://github.com/probe-rs/probe-rs/compare/v0.16.0...master
[v0.16.0]: https://github.com/probe-rs/probe-rs/compare/v0.15.0...v0.16.0
[v0.15.0]: https://github.com/probe-rs/probe-rs/compare/v0.14.2...v0.15.0
[v0.14.2]: https://github.com/probe-rs/probe-rs/compare/v0.14.1...v0.14.2
[v0.14.1]: https://github.com/probe-rs/probe-rs/compare/v0.13.0...v0.14.1
[0.13.0]: https://github.com/probe-rs/probe-rs/releases/tag/v0.13.0
