# Releasing probe-rs Crates

For a release the following steps are required:

1. Run `cargo xtask fetch-prs` in the respective repo. This will open a list of all merged GH PRs since the last release that have not yet received a changelog entry. Make sure all PRs from that list are included in the `CHANGELOG.md` and remove the `needs-changelog` label from the PRs as you go.

2. Run `cargo xtask release <version>` in the respective repo with `version` being the version that this release is assigned. This triggers a pipeline which bumps all dependencies, rotates the changelog and creates a commit with the changes on a branch for that release. It then creates a new PR with a `release` label for GHA to automatically release that version when it is successfully merged into `master`.

3. Update the versions in the `CHANGELOG.md` accordingly.

4. Let GHA take its course.

5. Optionally, fix issues on the created PR. But of course, we do not make mistakes here.

6. Merge the PR and wait until the release pipeline is done. Pay attention to potential issues.

# Required changelog entry for PRs

Generally we require a changelog entry for each PR. If for good reason it is omitted but required for the release, please add a `needs-changelog` label to the PR to make the CI pass. If it's a purely maintenance related PR,, you can also use `skip-changelog` to skip putting a changelog entry now and lateron during the release.

# The release scripts

There are two workflows:

**release.yml**

This workflow runs when a PR gets merged onto master successfully and has the `release` tag. For it to run successfully it expects the release branch to be named `release/<version>`.

**start-release.yml**

This workflow can be manually triggered from the Github UI or via the CLI with `gh workflow run 'Open a release PR' --ref master -f version=<version>`. It automatically bumps the versions, rotates the changelog and commits the changes.
