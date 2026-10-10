//! Duplicate, subsumed, and near-duplicate tests over coverage classes.

use std::cmp::Ordering;
use std::collections::HashMap;

use super::coverage::Coverage;
use super::{Candidate, CandidateKind, Relation};
use crate::ids::{ClassIdx, TestIdx};

/// Duplicates and subsumed tests by relative gap, then near duplicates by
/// Jaccard. `same_file` is left false.
pub(crate) fn find(cov: &Coverage, near_threshold: f64) -> Vec<Candidate> {
    let tests = &cov.tests;
    let mut candidates = Vec::new();

    let mut groups: HashMap<&[ClassIdx], Vec<usize>> = HashMap::new();
    for (i, t) in tests.iter().enumerate() {
        if t.region_count > 0 {
            groups.entry(t.classes.as_slice()).or_default().push(i);
        }
    }
    let mut dropped = vec![false; tests.len()];
    for members in groups.values().filter(|m| m.len() > 1) {
        // Keep a unit test when one exists: it fails faster and closer to the cause.
        let keeper = *members
            .iter()
            .min_by_key(|&&i| (tests[i].tier(), &tests[i].id))
            .expect("group is non-empty");
        for &i in members.iter().filter(|&&i| i != keeper) {
            dropped[i] = true;
            candidates.push(Candidate {
                relation: Relation::Duplicate {
                    group_size: members.len() as u32,
                },
                test: TestIdx::new(i),
                other: TestIdx::new(keeper),
                shared: tests[i].region_count,
                gap: 0,
                extra: 0,
                same_file: false,
            });
        }
    }

    // A dropped duplicate is listed once, as a duplicate. Witnesses are keepers
    // or unique tests, so the report never points at a test it also flags as a duplicate.
    for (i, t) in tests.iter().enumerate() {
        if t.region_count == 0 || dropped[i] {
            continue;
        }
        let strict = strict_supersets(cov, i, &dropped);
        let witness = strict.iter().copied().min_by_key(|&j| {
            let s = &tests[j];
            (s.region_count, s.tier() != t.tier(), &s.id)
        });
        let Some(witness) = witness else { continue };
        candidates.push(Candidate {
            relation: Relation::Subsumed {
                strict_supersets: strict.len() as u32,
            },
            test: TestIdx::new(i),
            other: TestIdx::new(witness),
            shared: t.region_count,
            gap: tests[witness].region_count - t.region_count,
            extra: 0,
            same_file: false,
        });
    }

    let gap_ratio = |c: &Candidate| f64::from(c.gap) / f64::from(tests[c.other.get()].region_count);
    candidates.sort_by(|a, b| {
        gap_ratio(a)
            .total_cmp(&gap_ratio(b))
            .then_with(|| {
                (a.kind() != CandidateKind::Duplicate).cmp(&(b.kind() != CandidateKind::Duplicate))
            })
            .then_with(|| tests[a.test.get()].id.cmp(&tests[b.test.get()].id))
    });
    let mut near = near_pairs(cov, near_threshold, &dropped);
    near.sort_by(|a, b| {
        b.jaccard()
            .total_cmp(&a.jaccard())
            .then_with(|| tests[a.test.get()].id.cmp(&tests[b.test.get()].id))
    });
    candidates.extend(near);
    candidates
}

/// Tests with a strictly larger coverage set that contains test `index`'s set,
/// skipping dropped duplicates. Intersects class postings rarest first, and
/// starts from eligible tests only, so a large duplicate group never enters.
fn strict_supersets(cov: &Coverage, index: usize, dropped: &[bool]) -> Vec<usize> {
    let size = cov.tests[index].region_count;
    let mut classes = cov.tests[index].classes.clone();
    classes.sort_by_key(|c| cov.postings[c.get()].len());
    let Some((rarest, rest)) = classes.split_first() else {
        return Vec::new();
    };
    let mut acc: Vec<TestIdx> = cov.postings[rarest.get()]
        .iter()
        .copied()
        .filter(|t| !dropped[t.get()] && cov.tests[t.get()].region_count > size)
        .collect();
    for c in rest {
        if acc.is_empty() {
            break;
        }
        let posting = &cov.postings[c.get()];
        acc.retain(|t| posting.binary_search(t).is_ok());
    }
    acc.into_iter().map(TestIdx::get).collect()
}

/// Pairs with Jaccard >= threshold where neither set contains the other.
///
/// Prefix filter: if J(A, B) >= t then B shares at least t * |A| regions with
/// A, so B must cover one of A's rarest classes whose weights sum past
/// (1 - t) * |A|. Only tests in that union are compared exactly.
fn near_pairs(cov: &Coverage, threshold: f64, dropped: &[bool]) -> Vec<Candidate> {
    let tests = &cov.tests;
    let weight = |c: &ClassIdx| cov.classes[c.get()].len() as u32;
    let mut out = Vec::new();
    // seen[b] == a: b is already in a's union. Avoids materializing one entry
    // per (class, test) when big groups share many rare classes.
    let mut seen = vec![usize::MAX; tests.len()];
    for (a, ta) in tests.iter().enumerate() {
        if ta.region_count == 0 || dropped[a] {
            continue;
        }
        let wa = ta.region_count;
        let budget = (1.0 - threshold) * f64::from(wa);
        let in_window = |wb: u32| {
            threshold * f64::from(wa) <= f64::from(wb) && f64::from(wb) <= f64::from(wa) / threshold
        };
        let mut rarest = ta.classes.clone();
        rarest.sort_by_key(|c| cov.postings[c.get()].len());
        let mut acc = 0;
        let mut others: Vec<TestIdx> = Vec::new();
        for c in &rarest {
            let posting = &cov.postings[c.get()];
            // Each pair once: only b > a. Postings are ascending.
            for &b in &posting[posting.partition_point(|b| b.get() <= a)..] {
                let bi = b.get();
                if seen[bi] != a && !dropped[bi] && in_window(tests[bi].region_count) {
                    seen[bi] = a;
                    others.push(b);
                }
            }
            acc += weight(c);
            if f64::from(acc) > budget {
                break;
            }
        }
        others.sort_unstable();
        for b in others {
            let tb = &tests[b.get()];
            let wb = tb.region_count;
            let shared = overlap(&ta.classes, &tb.classes, weight);
            if shared == wa.min(wb) {
                continue; // subset or duplicate: reported above
            }
            if f64::from(shared) / f64::from(wa + wb - shared) < threshold {
                continue;
            }
            let (cand, other) = if (wa, &ta.id) <= (wb, &tb.id) {
                (TestIdx::new(a), b)
            } else {
                (b, TestIdx::new(a))
            };
            let (wc, wo) = (
                tests[cand.get()].region_count,
                tests[other.get()].region_count,
            );
            out.push(Candidate {
                relation: Relation::Near,
                test: cand,
                other,
                shared,
                gap: wo - shared,
                extra: wc - shared,
                same_file: false,
            });
        }
    }
    out
}

/// Total weight of the classes in both ascending lists.
fn overlap(a: &[ClassIdx], b: &[ClassIdx], weight: impl Fn(&ClassIdx) -> u32) -> u32 {
    let (mut i, mut j, mut sum) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            Ordering::Less => i += 1,
            Ordering::Greater => j += 1,
            Ordering::Equal => {
                sum += weight(&a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    sum
}
