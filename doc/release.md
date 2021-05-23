# Prerequisites for running probe-rs release scripts

Install `cargo-workspaces` (`cargo install cargo-workspaces`) and the GH cli (see https://github.com/cli/cli for install instructions).

# Releasing probe-rs Crates

For a release the following steps are required:

Repeat for the [probe-rs, probe-rs-rtt, cargo-embed, cargo-flash] repositories:

1. Run `cargo xtask fetch-prs` in the respective repo. This will opem a list of all merged GH PRs since the last release that have not yet received a changelog entry. Make sure all PRs from that list are included in the `CHANGELOG.md` and remove the `needs-changelog` label from the PRs as you go.

2. Run `cargo xtask release <version>` in the respective repo with `version` being the version that this release is assigned. This will checkout master, pull all changes, bump all dependencies and create a commit with the changes on a branch for that release for you. It then creates a new PR with a `release-ready` label for GHA to automatically release that version when it is successfully merged into `master`.

3. Let GHA take its course.