# Collect: setup and troubleshooting

Reference for step 1 of the dead-weight-tests skill. Read it when `dwd collect` fails, when `skipped.json` is not empty, or when the project's `.config/nextest.toml` defines wrapper or setup scripts.

## Toolchain

Run `dwd doctor` before `dwd collect`. It checks the workspace, cargo-nextest, cargo-llvm-cov, the llvm-tools of the toolchain the workspace selects, the target dir, the dev shell, nextest wrapper scripts, and the artifact dir. It prints one line per check and the fix for each problem. It exits 1 when a check fails; warnings do not fail it. `dwd collect` runs the same tool and target-dir checks and stops at the first failure.

The target dir check fails when the workspace's target dir is a symlink to a missing directory. A target manager such as mbx leaves one after it evicts its store. `dwd collect` cannot create `<target>/dwd` under it. Recreate the destination the `fix:` line names, or set `CARGO_TARGET_DIR` to a real directory.

A project that builds only inside its dev shell (system libraries, a C SDK) needs every `dwd` command inside that shell.

## Nextest wrapper scripts

Each test gets its own profile through a nextest target runner. Tests behind a wrapper script write no coverage unless the wrapper has `target-runner = "within-wrapper"`. Keep the repo config untouched: copy it to an overlay, add the setting there, and pass the overlay through `--nextest`:

```sh
cp .config/nextest.toml /tmp/nextest-overlay.toml
# In the overlay, add target-runner = "within-wrapper" to each [scripts.wrapper.*] table.
dwd collect --nextest 'cargo nextest run --config-file /tmp/nextest-overlay.toml' -- --workspace
```

A `--nextest` wrapper command must pass its arguments through to `cargo nextest run`.

## Setup scripts

Setup scripts inherit `RUSTC_WRAPPER`. A setup script that builds another target, such as wasm32, fails with `can't find crate for profiler_builtins`. In the overlay, prefix its command with `env -u RUSTC_WRAPPER`.

## Binaries from another package

Tests that spawn a binary from another workspace package get coverage from it only when that binary is built with coverage. Build it with `--before`, which runs with `bash -c` after coverage is on and before the tests:

```sh
dwd collect --before 'cargo build -p my-cli' -- -p my-crate
```

## Skipped tests

`skipped.json` lists tests left out because their coverage is partial or missing. Each has one `reason`:

| reason | Meaning | Fix |
| --- | --- | --- |
| `failed` | The test exited non-zero (`exit` holds the code). | Fix or exclude the test; its coverage is partial. |
| `killed` | Nextest killed the runner, usually a timeout. | Instrumented tests run slower: raise `slow-timeout` in an overlay. |
| `no_profile` | The test ran but wrote no profile. | Look for a wrapper script (above) or `env_clear()`. |
| `never_reached` | Nextest selected the test, but it never reached the runner. | Usually a wrapper script (above). |
| `export_failed` | `llvm-profdata` or `llvm-cov` failed; `message` holds the error. | Check the toolchain section. |

## Instrumented build artifacts

`dwd collect` builds instrumented code in `<target>/dwd`, where `<target>` is the workspace's target directory from `cargo metadata` (it honors `CARGO_TARGET_DIR` and `build.target-dir`). It sets both `CARGO_TARGET_DIR` and `CARGO_BUILD_BUILD_DIR` to that directory, so a `build.build-dir` setting does not share fingerprints with normal builds. Normal builds never see those artifacts, and both builds stay incremental across runs.

The coverage wrapper flags sit outside cargo's fingerprint. So `dwd collect` keeps a stamp file, `<target>/dwd/dwd-flags`, with the wrapper variables that affect codegen. When the stamp is missing or differs, for example after a cargo-llvm-cov upgrade or a change to the workspace crate list, it runs `cargo llvm-cov clean --workspace` in that directory first. Delete `<target>/dwd` to force a full instrumented rebuild.

Tests that hardcode `target/debug/<bin>` paths no longer find binaries built during a collection. `CARGO_BIN_EXE_*` and lookups relative to the test executable still work.
