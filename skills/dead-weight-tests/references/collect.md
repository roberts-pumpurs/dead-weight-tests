# Collect: setup and troubleshooting

Reference for step 1 of the dead-weight-tests skill. Read it when `dwd collect` fails, when `skipped.json` is not empty, or when the project's `.config/nextest.toml` defines wrapper or setup scripts.

## Toolchain

`dwd collect` looks for `llvm-cov` and `llvm-profdata` in the sysroot of the toolchain the workspace selects, before it runs any test. When they are missing, it stops and names the path it checked.

- A workspace that pins a toolchain in `rust-toolchain.toml` needs the component for that toolchain: run `rustup component add llvm-tools-preview` inside the workspace.
- A Nix dev shell needs `llvm-tools-preview` in its rust-overlay or fenix toolchain.
- A project that builds only inside its dev shell (system libraries, a C SDK): run every `dwd` command inside that shell.

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

The coverage wrapper sits outside cargo's fingerprint. So `dwd collect` runs `cargo llvm-cov clean --workspace` before the tests, so that workspace crates rebuild with coverage, and again when it ends (also on errors, Ctrl-C, and SIGTERM), so that the next normal build rebuilds them without coverage. A copy of `target/` taken during a collection holds instrumented artifacts that a later build would reuse: copy `target/` before or after a collection.
