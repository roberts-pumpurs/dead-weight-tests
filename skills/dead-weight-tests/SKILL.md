---
name: dead-weight-tests
description: >-
  Find dead-weight tests in a Rust workspace that runs cargo-nextest: tests whose coverage duplicates, sits inside, or nearly matches another test's. Use when asked for dead-weight, redundant, or deletable tests, or when handed a review prompt copied from a dwd report.
metadata:
  version: "0.2.2" # x-release-please-version
---

# Dead-weight tests

Coverage finds suspects. You decide each verdict by reading both tests. The `dwd` CLI does the coverage work.

## The run

The user installed only this skill. You set up everything else, then run the analysis.

- **Find dead weight**: the user asks for dead-weight, redundant, or deletable tests. Run steps 0 to 5 in order. The run is done when you present the step 5 summary. Step 6 runs when the user approves changes.
- **Review prompt**: the user pastes a prompt copied from a dwd report. It lists candidates and a complete `dwd … verdict --expect '…'` command. Run step 0, apply step 3 to every listed candidate, then pipe the verdicts into that exact command (step 4). The run is done when it exits 0. Then present step 5, scoped to the candidates you recorded.

Verdicts persist per workspace. A new run on a workspace with earlier verdicts collects again (it is incremental), and `dwd pending` then lists only the pairs without a current verdict. A verdict whose test body changed since it was recorded comes back as pending.

## 0. Set up

Do this at the start of every run. It is fast when everything is in place.

1. Run `dwd --version`. It must print `dwd 0.2.2`. <!-- x-release-please-version -->
   When the command is missing or prints another version, install the matching `dwd`. Tell the user first: it compiles from source and takes a few minutes.

   ```sh
   cargo install --locked --force --git https://github.com/roberts-pumpurs/dead-weight-tests --tag v0.2.2 dead-weight-tests # x-release-please-version
   ```

   No `cargo`? Stop and tell the user to install Rust from <https://rustup.rs>.

2. Run `dwd doctor` inside the workspace. It checks the workspace, `cargo-nextest`, `cargo-llvm-cov`, the LLVM tools of the workspace's toolchain, the dev shell, and nextest wrapper scripts. It prints one `ok`, `warn`, or `fail` line per check, and a `fix:` line under each warning and failure. It exits 1 when a check fails.
   - Run every fix command it prints, then run `dwd doctor` again. Repeat until it exits 0.
   - A dev-shell warning means the project builds inside a dev shell. Its `fix:` line names how to enter it (`direnv exec .`, `nix develop -c`, or `nix-shell --run`). Run every later `dwd` command that way, `dwd doctor` included.
   - A wrapper-script warning needs an overlay config. Read [references/collect.md](references/collect.md) before step 1.
   - Its `artifacts` line shows earlier verdicts for this workspace. Mention them to the user.

Setup is done when `dwd doctor` exits 0 and you have handled each warning.

## Terms

- **Region**: an LLVM code region: file, start line and column, end line and column.
- **Test code**: files under `tests/` or `benches/`, `test.rs`/`tests.rs`/`*_test.rs`/`*_tests.rs` files, and `#[cfg(test)]` modules. The analysis ignores it, because every test runs its own body.
- **Coverage set**: the non-test-code regions one test executes, including regions run by workspace binaries it spawns.
- **Duplicate**: a test with the same coverage set as another test. The **keeper** is the one the analysis keeps (a unit test when the group has one).
- **Subsumed**: a test whose coverage set is a strict subset of another test's. The **witness** is the smallest such test.
- **Near duplicate**: two tests whose coverage sets have a Jaccard similarity of at least `--near` (default 0.95), where neither contains the other. The smaller test is the candidate; the other is its **twin**.
- **Gap**: the regions the other test executes beyond the candidate.
- **Candidate**: a duplicate, subsumed, or near-duplicate test. A candidate is a suspect, not a verdict.
- **Verdict**: cut, merge, keep, or fix for one candidate, with a one-sentence reason.

## 1. Collect coverage

Run from anywhere inside the workspace, or pass `-C DIR`. Arguments after `--` go to `cargo nextest run`:

```sh
dwd collect -- -p my-crate                      # one package
dwd collect -- --workspace -E 'test(/parser/)'  # a filterset
dwd collect -- --workspace                      # every test nextest runs
```

- Pick the scope first. Every test needs its own `llvm-cov export`, so a scoped run is faster. A scoped run finds witnesses only inside its scope, so a test with no witness in scope may still be redundant. Run the whole workspace when the user asks for it.
- The step is done when `dwd collect` prints the artifact directory. Read [references/collect.md](references/collect.md) when collect fails, when it reports skipped tests, or when the project's `.config/nextest.toml` defines wrapper or setup scripts.
- Instrumented builds go to `<target>/dwd`, apart from normal builds. The first collect builds the workspace with coverage. Later collects rebuild only what changed.
- Artifacts, verdicts included, live in `$XDG_STATE_HOME/dwd/<workspace name>-<hash>/` (`~/.local/state/...` when the variable is unset). They survive reboots and a new collect. `--dir DIR` or `DWD_DIR` moves them. `dwd path [report|candidates|verdicts|…]` prints a file's path.

## 2. Analyze

```sh
dwd analyze
```

It prints a status summary: test and skipped counts, candidates by kind, reviewed and pending counts, the coverage that recorded cuts lose, and the path of `report.html`. It writes into the artifact directory:

- `report.md`: the same summary, a Mermaid containment graph, and the top candidates.
- `report.html`: the interactive report for the user. Offer it in every summary you give: `open "$(dwd path report)"`.
- `candidates.json`: every candidate. Prefer `dwd pending`, which drops the ones that already have a verdict.

Candidates are in review order everywhere: same-file duplicates and subsets first, because they are the cheapest to judge and yield the most cuts, then the other duplicates and subsets by smallest gap, then near duplicates by Jaccard.

## 3. Review

```sh
dwd pending             # the next 20 candidates without a current verdict, one JSON line each
dwd pending --limit 50
```

Each line is a `candidates.json` row: both test ids and locations, `kind`, `other_role` (keeper, witness, or twin), region counts, and `same_file`. A row with `previous` had a verdict, but one of the two test bodies changed since: judge it again, and say in the reason what changed. stderr says how many are pending, how many are same-file, and how many are stale. Empty stdout means every candidate has a current verdict.

For **Find dead weight**, review the first `dwd pending` batch, record it (step 4), then run `dwd pending` again. `dwd pending` only drops candidates that have a recorded verdict, so always record a batch before you ask for the next one. Take more batches while stderr still reports same-file candidates, or when the user asks. With more than about 30 candidates to review, split them into batches by source file and give each batch to a read-only subagent that applies these rules.

Coverage shows what a test executes, not what it asserts. For each candidate, open the candidate and its keeper, witness, or twin, and compare their assertions.

Recommend **cut** when the candidate's assertions are weaker than, or the same as, the other test's assertions on the same behavior. Typical LLM dead weight:

- asserts only that a call does not panic, or that output is non-empty
- re-checks a constructor, getter, default value, or serde round trip that the other test already asserts
- copies another test with renamed variables and the same inputs

Recommend **merge** when the candidate has one small distinct assertion that fits into the other test. Name the assertion to move. A merge moves one or two assertions. One keeper absorbs at most two merges: record keep or fix for the extras, and say why in the reason.

Recommend **fix** when the candidate's name or doc comment promises a check its body never makes, for example `parses_for_non_utc_offset` that never passes a non-UTC offset. Recommend rewriting the body to check the promise, or renaming the test. A fix keeps the test.

Recommend **keep** and state the reason when any of these is true:

- It asserts a different input class: a boundary, an error variant, an empty or maximum value. Same regions, different behavior.
- It is a unit test for logic that the other test reaches only through a slow integration path, and its failure points closer to the cause.
- The other test needs services, a browser, or an env gate, so it does not run in every environment that runs the candidate.
- The candidate has no covered non-test regions because an env gate skipped it. Coverage says nothing about it.

Example: `error_names_the_offending_stage` is a duplicate of `malformed_paths_are_rejected`: both run the same regions. The keeper only checks `is_err()`. The duplicate checks the stage index and the message text. Keep it.

For a near duplicate, read the regions each side adds (`report.md` names their files). If the candidate's extra regions carry no distinct assertion, judge it like a subsumed test.

Each case of a parametrized test (rstest, test-case) is its own test id, `…::fn_name::case_name`, and its location points at the parametrized function. Two cases of one function usually differ in input class: compare the case arguments, not the shared body. When a location is ambiguous or missing (other macro-generated tests), find the test by name before you judge it.

## 4. Record verdicts

Record every reviewed candidate with `dwd verdict`, which validates each row before it appends it to the ledger. One JSON line per candidate:

```json
{"test": "<candidate id>", "other": "<other id>", "verdict": "cut|merge|keep|fix", "reason": "<one sentence naming the concrete assertion difference>"}
```

A `dwd pending` line with `verdict` and `reason` added is also a valid row: other fields are ignored.

```sh
dwd verdict my-verdicts.jsonl   # or pipe the lines on stdin; a JSON array also works
```

- It appends rows whose pair is a candidate, whose verdict is valid, and whose reason is not empty. Later lines win for the same pair.
- It prints one JSON object: `recorded`, `invalid` (line number and error), `missing`, `pending` (candidates without a current verdict), and `coverage_loss`.
- It exits 1 if any row is invalid or any `--expect` pair is missing. Correct those rows and run it again until it exits 0.
- `--expect '<JSON array of {"test", "other"}>'` names pairs that must have a current verdict afterwards. A copied review prompt already contains it.
- Each recorded row also stores a hash of both test bodies. When a later collect sees a different body, the verdict turns stale: it no longer counts, and `dwd pending` lists the pair again with `previous`.
- `coverage_loss` treats every test with a cut or merge verdict as deleted. It lists the regions no remaining test executes: `lost_regions`, the top `files`, and the deleted `tests` that held them. A near-duplicate cut loses its extra regions by design. Any other test in `tests` means two cuts depend on each other, for example a cut whose witness is also cut: change one of them to keep, or say so in the cut table.
- `dwd verdict` re-renders `report.md` and `report.html`, so the user sees the verdicts after a page reload.

## 5. Present

Show the user:

1. The run scope, test counts, and skipped count from the status summary.
2. The containment graph for the candidates you reviewed: the Mermaid graph from `report.md`, or the graph view of `report.html`.
3. A table of recommended cuts:

   | Test | Location | Kind | Witness / keeper | Why it is dead weight |
   | --- | --- | --- | --- | --- |

4. The coverage loss of all cuts and merges together. When `lost_regions` is not 0, name the files and the tests that hold the lost regions.
5. A separate list of **fix** recommendations: the test, what its name promises, and what the body checks instead. These are not cuts.
6. A short list of candidates you recommend keeping, each with its reason.
7. The report link: `open "$(dwd path report)"`.
8. **Next**: exactly one action, the first that applies from this list:
   1. Invalid rows or missing pairs from `dwd verdict`: fix and record them.
   2. Cuts with coverage loss that is not a near-duplicate extra: resolve the dependent cuts.
   3. Recorded cuts and merges: ask the user to approve them, then run step 6.
   4. `dwd pending` still reports same-file candidates: review the next batch.
   5. Other pending candidates: review the next `dwd pending` batch.
   6. Nothing pending in a scoped run: collect a wider scope, and name it.

   Say what it is, how many tests or candidates it touches, and the command or prompt that starts it.

This step changes no test. Cut tests only after the user approves the list.

## 6. Apply approved changes

Only after the user approves, and only the approved rows. Record each row the user rejects as `keep`, with the user's reason, so it leaves the cut list. Then:

1. Delete each approved cut. For a merge, move the named assertion into the other test, then delete the candidate. For a fix, rewrite the body to check what the name promises, or rename the test.
2. Run the affected tests with `cargo nextest run` and fix what you broke.
3. Run `dwd collect` with the same scope, then `dwd analyze`. Deleted tests drop out of the candidates. Fixed tests have a new body, so their verdicts turn stale and come back in `dwd pending`.
4. Present step 5 again, with the new counts and the next action.

## Limits

- Regions are matched exactly. Code compiled with different features in different binaries may yield different regions for the same source.
- Code behind test-only Cargo features in production files counts as covered code.
- Nondeterministic paths (scheduler order, retries, random `HashMap` order) can add or drop a few regions. Near-duplicate detection catches some of these pairs.
- Doctests are not included: nextest does not run them. Tests run on the host target only.
- Children spawned with `env_clear()` lose `LLVM_PROFILE_FILE`. Their coverage is missing, and they write `default_*.profraw` into their working directory, which can break tests that inspect that directory.
- Unix only.
- `dwd collect` builds into `<target>/dwd`. Tests that hardcode `target/debug/<bin>` paths do not find binaries built during a collection. `CARGO_BIN_EXE_*` still works.
