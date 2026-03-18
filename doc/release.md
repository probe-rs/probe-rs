# OpenSiFli Release and Upstream Sync

`OpenSiFli/master` is the public release branch for this fork.

- Do not periodically rebase `OpenSiFli/master`.
- Sync upstream changes from `probe-rs` with a dedicated merge commit.
- Prefer syncing to upstream release tags. Only sync `probe-rs/master` directly when you explicitly need an unreleased upstream fix.

## Syncing a new upstream release

1. Fetch the latest upstream state and prepare a sync branch:

   `cargo xtask sync-upstream <upstream-tag>`

   Example:

   `cargo xtask sync-upstream v0.30.0`

2. If you want to inspect the commands first, use:

   `cargo xtask sync-upstream <upstream-tag> --dry-run`

3. Resolve merge conflicts on the generated `sync/<upstream-ref>` branch and refresh any OpenSiFli-only patches that still need to live downstream.

4. Run CI and the SiFli-specific smoke checks on the sync branch.

5. Open a PR from the sync branch into `OpenSiFli/master`. Keep the merge commit created by `sync-upstream`; it is the audit point that shows which upstream version was pulled in.

6. After the PR is merged, `OpenSiFli/master` becomes the new release base.

## Releasing from OpenSiFli/master

Once the sync PR is merged and `OpenSiFli/master` is green, run the release flow:

1. Run `cargo xtask fetch-prs` and make sure the changelog fragments are complete.

2. Run `cargo xtask release <version>` from `master` or from the matching `release/x.y` branch for patch releases.

3. Review the generated release PR, fix anything that failed validation, then merge it.

4. After the release PR merges, GitHub Actions will:

   - create and push the version tags via `release_crates.yml`
   - build and publish release artifacts via `v-release.yml`
   - sync those artifacts to the SiFli mirror

## Required changelog entry for PRs

Generally we require a changelog entry for each PR. If for good reason it is omitted but still required for the release, please add a `changelog:need` label to make CI pass. If the change is purely maintenance-related, use `changelog:skip`.

## Automation reference

- `start-release.yml`: manually opens a release PR after bumping versions and rotating the changelog.
- `release_crates.yml`: creates and pushes the release tags after the release PR is merged.
- `v-release.yml`: builds distributable artifacts, publishes the GitHub release, and mirrors the release payload to SiFli.
