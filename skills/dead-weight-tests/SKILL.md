---
name: dead-weight-tests
description: >-
  Use when looking for redundant, low-value, or LLM-generated tests to cut in a Rust project that runs tests with cargo-nextest. Triggers include "dead weight tests", "redundant tests", "test redundancy", "which tests can we delete", "tests covered by other tests", or per-test coverage analysis. Collects per-test region coverage, finds tests whose coverage duplicates, is contained in, or nearly matches another test, renders an interactive HTML report, and records reviewed cut verdicts.
metadata:
  version: "0.2.0" # x-release-please-version
---

# Dead-weight tests

Find tests that execute nothing another test does not already execute. Coverage finds the suspects. You decide each verdict by reading the tests.

`<skill-dir>` below is the directory that contains this file.

## Terms

- **Region**: an LLVM code region: file, start line and column, end line and column.
- **Test code**: files under `tests/` or `benches/`, `tests.rs`/`*_tests.rs`/`*_test.rs` files, and `#[cfg(test)]` modules. The analyzer ignores it, because every test runs its own body.
- **Coverage set**: the non-test-code regions one test executes, including regions run by workspace binaries it spawns.
- **Duplicate**: a test with the same coverage set as another test. The **keeper** is the one the analyzer keeps (a unit test when the group has one).
- **Subsumed**: a test whose coverage set is a strict subset of another test's. The **witness** is the smallest such test.
- **Near duplicate**: two tests whose coverage sets have a Jaccard similarity of at least `--near` (default 0.95), where neither contains the other. The smaller test is the candidate; the other is its **twin**.
- **Gap**: the regions the other test executes beyond the candidate.
- **Candidate**: a duplicate, subsumed, or near-duplicate test. A candidate is a suspect, not a verdict.
- **Verdict**: cut, merge, keep, or fix for one candidate, with a one-sentence reason, stored in `verdicts.jsonl`.

## 1. Collect coverage

Requirements: `cargo-nextest`, `cargo-llvm-cov`, the `llvm-tools-preview` rustup component, and `python3`. Run from the workspace root:

```sh
<skill-dir>/scripts/collect.sh -- --workspace                      # every test nextest runs
<skill-dir>/scripts/collect.sh -- -p my-crate                      # one package
<skill-dir>/scripts/collect.sh -- --workspace -E 'test(/parser/)'  # a filterset
```

- Arguments after `--` go to `cargo nextest run`. `collect.sh` adds `--no-fail-fast`.
- `--nextest CMD` replaces `cargo nextest run`, for projects that wrap nextest in a script. The wrapper must pass its arguments through to `cargo nextest run`.
- `--before CMD` runs after coverage is enabled and before the tests. Use it to build binaries that tests spawn from another package, so those binaries are instrumented too.
- Each test gets its own profile through a nextest target runner. If the project's `.config/nextest.toml` defines wrapper scripts, tests behind a wrapper write no coverage unless each wrapper has `target-runner = "within-wrapper"`. Do not edit the repo config. Copy it to an overlay, add the setting there, and pass the overlay through `--nextest`:

  ```sh
  mkdir -p target/coverage
  cp .config/nextest.toml target/coverage/nextest-overlay.toml
  # In the overlay, add target-runner = "within-wrapper" to each [scripts.wrapper.*] table.
  <skill-dir>/scripts/collect.sh \
    --nextest 'cargo nextest run --config-file target/coverage/nextest-overlay.toml' -- --workspace
  ```

- Setup scripts inherit `RUSTC_WRAPPER`. A setup script that builds another target, such as wasm32, fails with `can't find crate for profiler_builtins`. In the overlay, prefix its command with `env -u RUSTC_WRAPPER`.
- Instrumented tests run slower. Raise `slow-timeout` in the overlay for slow test groups, or they time out and land in `skipped.json`.
- A scoped run only finds witnesses inside its scope. A test with no witness in scope may still be redundant. Prefer a scoped run unless the user asks for the whole workspace: every test needs its own `llvm-cov export`.
- `collect.sh` first runs `cargo llvm-cov clean --workspace`: the coverage wrapper sits outside cargo's fingerprint, so artifacts from a normal build would otherwise be reused without instrumentation. Workspace crates rebuild with coverage. If a nextest wrapper needs prebuilt workspace binaries, build them with `--before`.
- At the end of the run, after export, `collect.sh` removes the instrumented workspace artifacts inside the coverage environment. The next normal build then rebuilds workspace crates. Do not copy or clone `target/` while a collection runs: the copy holds instrumented artifacts that a later build would reuse.
- Output: `target/coverage/per-test/tests/*.json.gz` (one pruned `llvm-cov export` JSON per passing test) and `skipped.json` (failed or killed tests, and tests that never reached the runner, excluded because their coverage is partial or missing).

## 2. Analyze and render

```sh
python3 <skill-dir>/scripts/analyze.py target/coverage/per-test/tests \
  --out target/coverage/per-test/report.md \
  --json target/coverage/per-test/candidates.json \
  --html target/coverage/per-test/report.html
```

- `report.md`: summary, Mermaid graph, and the top `--limit` candidates (default 100), duplicates and subsumed by smallest gap, then near duplicates by Jaccard.
- `candidates.json`: every candidate, for agents. Each row has `same_file`: true when the candidate and the other test live in the same source file. The report shows it too. Review same-file duplicates and subsets first: they are the cheapest to judge and yield the most cuts.
- `report.html`: one self-contained page. It loads its graph and plotting libraries from a CDN. Views: overview treemap and candidate table, containment graph, pair view (both test bodies plus source lines colored by which test covers them), file view (tests per line), and test view (relations and an UpSet plot of overlaps). In the graph, click a test or edge to read its code; double-click opens the full page.
- The page shows verdicts from `target/coverage/per-test/verdicts.jsonl` (next to the `tests/` directory) when it exists.

## 3. Record verdicts lazily

Verdicts cost one review per candidate, so record them only for the candidates the user wants judged. The page has **Copy review prompt** buttons for one candidate and for the current filtered table. The user pastes the prompt into an agent. When you receive such a prompt, review each listed candidate with the rules in section 4 and record one line per candidate with `record_verdict.py`. Do not append to `verdicts.jsonl` by hand. Each line looks like this:

```json
{"test": "<candidate id>", "other": "<other id>", "verdict": "cut|merge|keep|fix", "reason": "<one sentence naming the concrete assertion difference>"}
```

Pipe the lines into the recorder:

```sh
python3 <skill-dir>/scripts/record_verdict.py --candidates target/coverage/per-test/candidates.json < my-verdicts.jsonl
```

It appends only rows whose pair is in `candidates.json`, whose verdict is valid, and whose reason is not empty. It prints JSON with `recorded`, `invalid` (line number and error), and `missing`, and exits 1 if any row is invalid. `--verdicts` defaults to `verdicts.jsonl` next to the candidates file. Pass `--expect FILE`, a JSON list of the candidate rows you were asked to judge, to list pairs that still have no verdict. Fix the reported rows and run it again.

Later lines win for the same pair. To show new verdicts, re-run the analyzer with `--html`, or load the file with the page's **Load verdicts.jsonl** button. For more than about 30 candidates, split them into batches by source file and review the batches in parallel with read-only subagents.

## 4. Review rules

Coverage shows what a test executes, not what it asserts. For each candidate, open the candidate and its witness, keeper, or twin, and compare them.

Recommend **cut** when the candidate's assertions are weaker than, or the same as, the other test's assertions on the same behavior. Typical LLM dead weight:

- asserts only that a call does not panic, or that output is non-empty
- re-checks a constructor, getter, default value, or serde round trip that the other test already asserts
- copies another test with renamed variables and the same inputs

Recommend **merge** when the candidate has one small distinct assertion that fits into the other test. Name the assertion to move. A merge moves one or two assertions. If one keeper would absorb more than two candidates, record keep or fix for the extras and say so in the reason. Do not fold them all into one test.

Recommend **fix** when the candidate's name or doc comment promises a check its body never makes, for example `parses_for_non_utc_offset` that never passes a non-UTC offset. Recommend rewriting the body to check the promise, or renaming the test. Do not cut it.

Recommend **keep** and state the reason when any of these is true:

- It asserts a different input class: a boundary, an error variant, an empty or maximum value. Same regions, different behavior.
- It is a unit test for logic that the other test reaches only through a slow integration path, and its failure points closer to the cause.
- The other test needs services, a browser, or an env gate, so it does not run in every environment that runs the candidate.
- The candidate has no covered non-test regions because an env gate skipped it. Coverage says nothing about it.

Example: `error_names_the_offending_stage` is a duplicate of `malformed_paths_are_rejected`: both run the same regions. The keeper only checks `is_err()`. The duplicate checks the stage index and the message text. Keep it.

For a near duplicate, read the regions each side adds (the report names their files). If the candidate's extra regions carry no distinct assertion, treat it like a subsumed test.

When the location is ambiguous or unknown (macro-generated tests), find the test by name before you judge it.

## 5. Present

Do not edit tests in this step. Show the user:

1. The run scope, test counts, and skipped count from the report header.
2. The containment graph for the candidates you reviewed (Mermaid from the report, or point the user to the HTML graph view).
3. A table of recommended cuts:

   | Test | Location | Kind | Witness / keeper | Why it is dead weight |
   | --- | --- | --- | --- | --- |

4. A separate list of **fix** recommendations: the test, what its name promises, and what the body checks instead. These are not cuts.
5. A short list of candidates you recommend keeping, each with its reason.
6. The next action: which cuts to make first, or which scope to run next.

Cut tests only after the user approves the list.

## Limits

- Regions are matched exactly. Code compiled with different features in different binaries may yield different regions for the same source.
- Code behind test-only Cargo features in production files counts as covered code.
- Nondeterministic paths (scheduler order, retries, random `HashMap` order) can add or drop a few regions. Near-duplicate detection catches some of these pairs.
- Doctests are not included: nextest does not run them.
- Children spawned with `env_clear()` lose `LLVM_PROFILE_FILE`. Their coverage is missing, and they write `default_*.profraw` into their working directory. That can break tests that inspect that directory.
