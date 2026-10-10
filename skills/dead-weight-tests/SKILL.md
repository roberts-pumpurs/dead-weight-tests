---
name: dead-weight-tests
description: >-
  Find dead-weight tests in a Rust workspace that runs cargo-nextest: tests whose coverage duplicates, sits inside, or nearly matches another test's. Use when asked for dead-weight, redundant, or deletable tests, or when handed a review prompt copied from a dwd report.
metadata:
  version: "0.2.1" # x-release-please-version
---

# Dead-weight tests

Coverage finds suspects. You decide each verdict by reading both tests. The `dwd` CLI does the coverage work.

Run `dwd --version` first. If the command is missing, install it:

```sh
cargo install --locked --git https://github.com/roberts-pumpurs/dead-weight-tests dead-weight-tests
```

`dwd` also needs `cargo-nextest`, `cargo-llvm-cov`, and the `llvm-tools-preview` component of the workspace's toolchain. `dwd collect` checks the toolchain and names the fix.

## Two ways in

- **Find dead weight**: the user asks for dead-weight, redundant, or deletable tests. Run steps 1 to 5 in order. The run is done when you present the step 5 summary.
- **Review prompt**: the user pastes a prompt copied from a dwd report. It lists candidates and a `dwd … verdict` command. Apply step 3 to every listed candidate, then record the verdicts with step 4, using the command from the prompt. The run is done when the recorder reports no invalid rows and no missing pairs. Then tell the user what you recorded.

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
- The artifact directory defaults to `<OS temp dir>/dwd/<workspace name>-<hash>/`, and the OS deletes it when it cleans its temp dir, verdicts included. To keep verdicts across reboots, pass the same `--dir DIR` (or set `DWD_DIR`) on every `dwd` command. `dwd path [report|candidates|verdicts|…]` prints a file's path.

## 2. Analyze

```sh
dwd analyze
```

It writes three reports into the artifact directory:

- `candidates.json`: every candidate with both test ids, locations, region counts, and `same_file`. Work from this file.
- `report.md`: summary counts, a Mermaid containment graph, and the top candidates.
- `report.html`: the interactive report for the user. Point them to `open "$(dwd path report)"`.

Candidates are in review order everywhere: same-file duplicates and subsets first, because they are the cheapest to judge and yield the most cuts, then the other duplicates and subsets by smallest gap, then near duplicates by Jaccard.

## 3. Review rules

For **Find dead weight**, review every same-file candidate, then continue in rank order until you have reviewed 20 candidates or reached the end of the list. Review more when the user asks. With more than about 30 candidates to review, split them into batches by source file and give each batch to a read-only subagent that applies these rules.

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

When the location is ambiguous or unknown (macro-generated tests), find the test by name before you judge it.

## 4. Record verdicts

Record every reviewed candidate with `dwd verdict`, which validates each row before it appends it to the ledger. One JSON line per candidate:

```json
{"test": "<candidate id>", "other": "<other id>", "verdict": "cut|merge|keep|fix", "reason": "<one sentence naming the concrete assertion difference>"}
```

```sh
dwd verdict my-verdicts.jsonl   # or pipe the lines on stdin; a JSON array also works
```

- It appends rows whose pair is a candidate, whose verdict is valid, and whose reason is not empty. It prints JSON with `recorded`, `invalid` (line number and error), and `missing`, and exits 1 if any row is invalid or any expected pair is missing.
- For a review prompt, write its pairs to a file as `[{"test": "…", "other": "…"}]` and pass `--expect FILE`: `missing` then lists the pairs that still have no verdict.
- Correct the reported rows and run it again until it exits 0. Later lines win for the same pair.
- `dwd verdict` re-renders `report.md` and `report.html`, so the user sees the verdicts after a page reload.

## 5. Present

Show the user:

1. The run scope, test counts, and skipped count from the `report.md` summary.
2. The containment graph for the candidates you reviewed: the Mermaid graph from `report.md`, or the graph view of `report.html`.
3. A table of recommended cuts:

   | Test | Location | Kind | Witness / keeper | Why it is dead weight |
   | --- | --- | --- | --- | --- |

4. A separate list of **fix** recommendations: the test, what its name promises, and what the body checks instead. These are not cuts.
5. A short list of candidates you recommend keeping, each with its reason.
6. The next action: which cuts to make first, or which scope to run next.

This step changes no test. Cut tests only after the user approves the list.

## Limits

- Regions are matched exactly. Code compiled with different features in different binaries may yield different regions for the same source.
- Code behind test-only Cargo features in production files counts as covered code.
- Nondeterministic paths (scheduler order, retries, random `HashMap` order) can add or drop a few regions. Near-duplicate detection catches some of these pairs.
- Doctests are not included: nextest does not run them. Tests run on the host target only.
- Children spawned with `env_clear()` lose `LLVM_PROFILE_FILE`. Their coverage is missing, and they write `default_*.profraw` into their working directory, which can break tests that inspect that directory.
- Unix only.
