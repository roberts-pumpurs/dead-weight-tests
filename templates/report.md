# Dead-weight test candidates

- Tests with coverage: {{ covered }} ({{ unit }} unit, {{ integration }} integration)
- Covered regions: {{ covered_regions }} in {{ covered_files }} files ({{ excluded }} test-code regions excluded)
- Skipped (failed or killed, partial coverage): {{ skipped.len() }}
- No covered non-test regions: {{ empty_count }}
- Duplicate candidates: {{ duplicates }}
- Subsumed candidates: {{ subsumed }}{{ subsumed_tiers }}
- Near duplicates (Jaccard >= {{ near_threshold }}): {{ near }}
- Same-file pairs (candidate and other in one source file, listed first): {{ same_file }}
- Reviewed: {{ reviewed }}. Pending: {{ pending }}{% if stale > 0 %} ({{ stale }} stale: a reviewed test body changed){% endif %}
{%- if let Some(line) = loss_line %}
- {{ line }}{% if !loss_files.is_empty() %}. Top files: {{ loss_files }}{% endif %}
{%- endif %}

Candidates are suspects. Coverage shows what a test executes, not what it asserts.
Read the candidate and the other test before cutting.
{%- if total > 0 %}

## Containment graph (top {{ graph_shown }})

`A --> B`: A's regions are a strict subset of B's; the label is B's extra regions. `===`: same regions. `-.-`: near duplicates, labelled with Jaccard.

```mermaid
{{ mermaid }}
```
{%- endif %}

## Candidates (top {{ candidates.len() }} of {{ total }}, in review order)
{%- for c in candidates %}

### {{ c.n }}. {{ c.kind }}{% if c.same_file %} (same file){% endif %}: `{{ c.test }}`

- Tier: {{ c.tier }}. Location: {{ c.location }}
- Covers {{ c.regions }} regions. Top files: {{ c.top_files }}
{%- if let Some(group_size) = c.group_size %}
- Keeper: `{{ c.other }}` ({{ c.other_tier }}, {{ c.other_location }})
- Reason: executes exactly the same {{ c.regions }} regions as the keeper. Group size: {{ group_size }}.
{%- else if let Some(supersets) = c.strict_supersets %}
- Witness: `{{ c.other }}` ({{ c.other_tier }}, {{ c.other_location }}) covers {{ c.other_regions }} regions
- Reason: all {{ c.regions }} regions of this test are also executed by the witness, which executes {{ c.gap }} more. Gap files: {{ c.diff_files }}
- Strict supersets in this run: {{ supersets }}
{%- else %}
- Near twin: `{{ c.other }}` ({{ c.other_tier }}, {{ c.other_location }}) covers {{ c.other_regions }} regions
- Reason: Jaccard {{ c.jaccard }}. They share {{ c.shared }} regions; this test adds {{ c.extra }} (in {{ c.diff_files }}), the twin adds {{ c.gap }}.
{%- endif %}
{%- match c.verdict %}
{%- when Some(Judged::Fresh(v)) %}
- Verdict: {{ v.verdict.as_str() }}. {{ v.reason }}
{%- when Some(Judged::Stale(v)) %}
- Verdict: {{ v.verdict.as_str() }} (stale: test body changed). {{ v.reason }}
{%- when None %}
{%- endmatch %}
{%- endfor %}
{%- if empty_count > 0 %}

## Tests with no covered non-test regions

Either an env gate skipped the test early, or it exercises only test code (helpers, fixtures). Check them by hand.
{% for t in shown_empty %}
- `{{ t }}`
{%- endfor %}
{%- endif %}
{%- if !skipped.is_empty() %}

## Skipped tests
{% for s in shown_skipped %}
- {{ s }}
{%- endfor %}
{%- endif %}
