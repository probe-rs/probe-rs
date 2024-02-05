# Contribution Guidelines

- Add tests and docs for any new functionality
- Format the code with [rustfmt](https://github.com/rust-lang/rustfmt)
  (Install with `rustup component add rustfmt`, run with `cargo fmt`) or use equivalent manual formatting.
- Use meaningful commit messages. You can follow the advice
  in [this blogpost](http://tbaggery.com/2008/04/19/a-note-about-git-commit-messages.html).

Thanks for your contributions :)


## Changelog entries

To avoid conflicts in the changelog file, the changelog is assembled out of fragments at release time. To describe the changes in a PR, add a new file in `changelog/`.
The filename should start with one of the following categories:

- added
- changed
- fixed
- removed

For example, `added-ultra-fast-flashing.md` would lead to an entry in the `## Added` section of the changelog. The content of the file should be a single line or paragraph describing your changes.

## How to build cargo-embed/ cargo-flash from source

`cargo-embed` is a so called [cargo subcommand](https://doc.rust-lang.org/book/ch14-05-extending-cargo.html). It is a programm named `cargo-embed` which is installed in the users path. Thus when applying some small fixes to `cargo-embed` you can run `cargo build --features cli` and then use the executable in the target folder named cargo-embed directly. You can also use [cargo install --path probe-rs --features cli](https://doc.rust-lang.org/cargo/commands/cargo-install.html) to install your current checkout locally overriding what you previously had installed using `cargo install cargo-embed`.

The steps are the same for cargo-embed or cargo-flash. Both use probe-rs inside and wrap it with a user friendly command line interface.

If you want to use a different version of probe-rs you can use [cargo patch](https://doc.rust-lang.org/edition-guide/rust-2018/cargo-and-crates-io/replacing-dependencies-with-patch.html) in your local clone of cargo-embed/ cargo-flash and set it to a specific version from Github or a local checkout of probe-rs. This is helpfull for testing patches.
