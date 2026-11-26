---
name: release
description: Prepare, publish, verify, or recover a Terraleph release. Use for version bumps, release tags, and incomplete distribution.
---

# Release Terraleph

A stable release reaches three destinations: GitHub binary archives and checksums, the Homebrew tap, and crates.io. README installation commands are part of this contract. Do not report completion based only on a tag, a GitHub release, or successful build jobs.

1. Before tagging, reconcile the README's install methods with `.github/workflows/release.yml` and ensure publishing access is available, including crates.io Trusted Publishing. Confirm the version in `Cargo.toml` and `Cargo.lock` and confirm CI on the exact commit to tag. Do not make unrelated edits or bypass failed security checks. Tag only with release authorization.
2. After the tag is pushed, follow the Release workflow through `Verify release distribution`. It checks all three destinations on stable releases. Prereleases use only GitHub; check that the prerelease and release assets are visible.
3. If a destination is missing, report it as partial publication and retry failed jobs on the same run as appropriate. Never move or recreate a published tag. For an existing stable GitHub/Homebrew release missing only crates.io, run `Release → Run workflow` on `main` with its existing tag once Trusted Publishing is configured; this recovery does not republish GitHub or Homebrew. If the crate version already exists but does not match the source commit or is yanked, stop and report the discrepancy instead of attempting to overwrite it.

Do not assume that fixing one distribution channel authorizes changing other projects, credentials, or the README.
