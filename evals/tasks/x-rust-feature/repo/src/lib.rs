/// Parse a duration like `1h30m`, `45s`, `2d4h` or `90m` into whole seconds.
///
/// Units are `d`, `h`, `m`, `s`. Each unit may appear at most once and they must be written
/// from largest to smallest. Returns `Err` with a short message for empty input, a missing
/// number, an unknown unit, a repeated or out-of-order unit, or a number without a unit.
pub fn parse_duration(input: &str) -> Result<u64, String> {
    let _ = input;
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_units() {
        assert_eq!(parse_duration("45s"), Ok(45));
        assert_eq!(parse_duration("90m"), Ok(5400));
        assert_eq!(parse_duration("2h"), Ok(7200));
        assert_eq!(parse_duration("1d"), Ok(86400));
    }

    #[test]
    fn combined() {
        assert_eq!(parse_duration("1h30m"), Ok(5400));
        assert_eq!(parse_duration("2d4h"), Ok(2 * 86400 + 4 * 3600));
        assert_eq!(parse_duration("1d1h1m1s"), Ok(86400 + 3600 + 60 + 1));
    }

    #[test]
    fn errors() {
        for bad in ["", "h", "10", "10x", "1h1h", "30m1h", "1h 30m", "-5s", "1.5h"] {
            assert!(parse_duration(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn large_values_do_not_wrap() {
        assert!(parse_duration("99999999999999999999s").is_err());
    }
}
