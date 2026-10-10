# Contributing

## Dev shell

`flake.nix` has a dev shell with everything the end-to-end test needs: the toolchain from `rust-toolchain.toml` (with `llvm-tools-preview`), `cargo-nextest`, and `cargo-llvm-cov`. With direnv, run `direnv allow` once; otherwise use `nix develop`.

## Checks

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo test --test fixture_e2e -- --ignored   # end to end, inside the dev shell
```

`fixture/` is the `dwd-fixture` workspace member, a small crate with planted dead weight. The table at the top of `fixture/src/lib.rs` lists the planted cases. The end-to-end test runs `dwd collect`, `analyze`, and `verdict` against it in a temporary target dir, checks that each planted case shows up, and checks that a normal build afterwards is not instrumented.

## Releases

Releases use [release-please](https://github.com/googleapis/release-please) and Conventional Commits: a `feat:` or `fix:` commit on `main` opens or updates the release PR. Merging the release PR bumps the `dead-weight-tests` version in `Cargo.toml`, `Cargo.lock`, and `SKILL.md` (not `dwd-fixture`), tags the version, attaches a skill archive to the GitHub release, and smoke-tests the release: it installs the skill with the skills CLI and `dwd` from the tag. skills.sh has no upload step: it lists public repositories from that install telemetry.

The release PR comes from the Actions token, and PRs from that token start no workflows. So the Release workflow dispatches CI on the release PR branch, and the required checks run on it like on any other PR.

## Branch rules

Branch rules for `main` live in [`.github/rulesets/main.json`](.github/rulesets/main.json): squash-merged PRs only, both CI jobs green, signed commits, no force pushes or deletion; repository admins can bypass. After a change to that file merges, apply it with an admin `gh` login: `.github/rulesets/apply.sh`. A required check that the file renames blocks every PR until you do.
