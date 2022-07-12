# Prerequisites for running probe-rs release scripts

Install `cargo-workspaces` (`cargo install cargo-workspaces`) and the GH cli (see https://github.com/cli/cli for install instructions).

# Releasing probe-rs Crates

For a release the following steps are required:

Repeat for the `[probe-rs, cargo-embed, cargo-flash]` repositories:

1. Run `cargo xtask fetch-prs` in the respective repo. This will open a list of all merged GH PRs since the last release that have not yet received a changelog entry. Make sure all PRs from that list are included in the `CHANGELOG.md` and remove the `needs-changelog` label from the PRs as you go.

2. Run `cargo xtask release <version>` in the respective repo with `version` being the version that this release is assigned. This will checkout master, pull all changes, bump all dependencies and create a commit with the changes on a branch for that release for you. It then creates a new PR with a `release` label for GHA to automatically release that version when it is successfully merged into `master`.

3. Update the versions in the `CHANGELOG.md` accordingly.

4. Mark the PR as ready for review.

5. Let GHA take its course.

6. Optionally, fix issues on the created PR. But of course, we do not make mistakes here.

7. Add the changelog to to the newly created Github release.

# Required changelog entry for PRs

Generally we require a changelog entry for each PR. If for good reason it is omitted but required for the release, please add a `needs-changelog` label to the PR to make the CI pass. If it's a purely maintenance related PR,, you can also use `skip-changelog` to skip putting a changelog entry now and lateron during the release.
