//! A small library with planted dead weight in its tests, used by the end-to-end check.

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
}
