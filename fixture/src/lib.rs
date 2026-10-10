//! A small library whose tests hold planted dead weight. `tests/fixture_e2e.rs`
//! runs `dwd` against this crate and expects each planted case in the report.
//!
//! | Planted case | Tests |
//! | --- | --- |
//! | duplicate | `parses_another_number` runs the same regions as `parses_a_number` |
//! | subsumed | `describes_a_positive_number` runs everything `classifies_positive` runs |
//! | near duplicate | `tallies_letters_and_spaces` and `tallies_letters_and_digits` differ by one branch |
//! | subprocess | `cli_describes_a_negative_number` reaches the library only through `describe` |
//! | failed | `fails_when_asked` fails when `DWD_FIXTURE_FAIL` is set |
//! | empty | `only_runs_test_code` executes no library code |

pub fn parse_number(s: &str) -> Result<i64, String> {
    let t = s.trim();
    if t.is_empty() {
        return Err("empty".into());
    }
    t.parse::<i64>().map_err(|e| format!("not a number: {e}"))
}

pub fn classify(n: i64) -> &'static str {
    if n < 0 {
        "negative"
    } else if n == 0 {
        "zero"
    } else {
        "positive"
    }
}

pub fn describe(s: &str) -> String {
    match parse_number(s) {
        Ok(n) => format!("{n} is {}", classify(n)),
        Err(e) => format!("error: {e}"),
    }
}

/// Character counts by class.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Tally {
    pub vowels: u32,
    pub consonants: u32,
    pub upper: u32,
    pub digits: u32,
    pub spaces: u32,
    pub punctuation: u32,
    pub other: u32,
}

/// Counts each character's class. One branch per class, so two inputs that
/// differ in one class cover nearly the same regions.
pub fn tally(s: &str) -> Tally {
    let mut t = Tally::default();
    for c in s.chars() {
        if matches!(c, 'a' | 'e' | 'i' | 'o' | 'u') {
            t.vowels += 1;
        } else if c.is_ascii_lowercase() {
            t.consonants += 1;
        } else if c.is_ascii_uppercase() {
            t.upper += 1;
        } else if c.is_ascii_digit() {
            t.digits += 1;
        } else if c == ' ' {
            t.spaces += 1;
        } else if c.is_ascii_punctuation() {
            t.punctuation += 1;
        } else {
            t.other += 1;
        }
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_number() {
        assert_eq!(parse_number("42"), Ok(42));
    }

    // Planted duplicate: the same path through `parse_number` as `parses_a_number`.
    #[test]
    fn parses_another_number() {
        assert_eq!(parse_number(" 7 "), Ok(7));
    }

    // Planted subsumed test: `describes_a_positive_number` runs everything this runs.
    #[test]
    fn classifies_positive() {
        assert_eq!(classify(5), "positive");
    }

    #[test]
    fn describes_a_positive_number() {
        assert_eq!(describe("5"), "5 is positive");
    }

    #[test]
    fn rejects_empty_input() {
        assert!(parse_number("").is_err());
    }

    #[test]
    fn tallies_letters_and_spaces() {
        let t = tally("Hi,all ");
        assert_eq!((t.upper, t.spaces), (1, 1));
    }

    #[test]
    fn tallies_letters_and_digits() {
        let t = tally("Hi,all7");
        assert_eq!((t.upper, t.digits), (1, 1));
    }

    // Planted failure: the end-to-end test sets DWD_FIXTURE_FAIL, so collect
    // must list this test as skipped with reason `failed`.
    #[test]
    fn fails_when_asked() {
        assert!(
            std::env::var_os("DWD_FIXTURE_FAIL").is_none(),
            "DWD_FIXTURE_FAIL is set"
        );
    }

    fn double(n: i64) -> i64 {
        n * 2
    }

    // Planted empty test: it runs only test code, so its coverage set is empty.
    #[test]
    fn only_runs_test_code() {
        assert_eq!(double(2), 4);
    }
}
