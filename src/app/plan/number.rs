#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct CanonicalNumber(String);

// The input must be JSON number text as serde_json scanned it from the plan, so the
// mantissa is already digits with an optional fraction. Text built from any other source
// must be validated at that boundary before it reaches here.
// Terraform and OpenTofu print numbers as plain decimals from big.Float, so only other
// notation reaches the i128 exponent limit. Values beyond it are not normalized, and
// callers handle them conservatively instead of comparing them as numbers.
pub(super) fn canonical_number(value: &str) -> Option<CanonicalNumber> {
    let (sign, unsigned) = value
        .strip_prefix('-')
        .map_or(("", value), |unsigned| ("-", unsigned));
    let (mantissa, exponent) = unsigned.split_once(['e', 'E']).unwrap_or((unsigned, "0"));
    let (integer, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let exponent = exponent.parse::<i128>().ok()?;

    let digits = format!("{integer}{fraction}");
    let significant = digits.trim_start_matches('0');
    if significant.is_empty() {
        return Some(CanonicalNumber("0".to_owned()));
    }
    let trimmed = significant.trim_end_matches('0');
    let shift = i128::try_from(significant.len() - trimmed.len()).ok()?
        - i128::try_from(fraction.len()).ok()?;
    let scale = exponent.checked_add(shift)?;
    Some(CanonicalNumber(format!("{sign}{trimmed}e{scale}")))
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::canonical_number;

    const I128_MAX: &str = "170141183460469231731687303715884105727";
    const I128_MAX_PLUS_ONE: &str = "170141183460469231731687303715884105728";
    const I128_MIN: &str = "-170141183460469231731687303715884105728";
    const I128_MIN_MINUS_ONE: &str = "-170141183460469231731687303715884105729";

    fn canonical(value: &str) -> Option<String> {
        canonical_number(value).map(|number| number.0)
    }

    #[test]
    fn canonicalizes_exact_digits_without_float_conversion() {
        struct NumberCase {
            name: &'static str,
            input: String,
            expected: String,
        }

        let long_integer = "12345678901234567890123456789012345678901234567890";
        let long_fraction = "98765432109876543210987654321098765432109876543219";
        let cases = [
            NumberCase {
                name: "integer above 2^53",
                input: "9007199254740993".to_owned(),
                expected: "9007199254740993e0".to_owned(),
            },
            NumberCase {
                name: "negative integer above 2^64",
                input: "-18446744073709551617".to_owned(),
                expected: "-18446744073709551617e0".to_owned(),
            },
            NumberCase {
                name: "mantissa longer than i128",
                input: format!("{long_integer}.{long_fraction}"),
                expected: format!("{long_integer}{long_fraction}e-50"),
            },
            NumberCase {
                name: "fractional trailing zeros",
                input: "-12.300e2".to_owned(),
                expected: "-123e1".to_owned(),
            },
            NumberCase {
                name: "long zero-padded positive exponent",
                input: format!("1e+{}5", "0".repeat(200)),
                expected: "1e5".to_owned(),
            },
            NumberCase {
                name: "long zero-padded negative exponent",
                input: format!("1E-{}5", "0".repeat(200)),
                expected: "1e-5".to_owned(),
            },
        ];

        for case in cases {
            assert_eq!(
                canonical(&case.input).as_deref(),
                Some(case.expected.as_str()),
                "case: {}",
                case.name
            );
        }
    }

    #[rstest]
    #[case::plain_integer("1500")]
    #[case::fraction_zeros("1500.000")]
    #[case::mantissa_fraction("1.5e3")]
    #[case::integer_mantissa("15e2")]
    #[case::uppercase_exponent("0.0015E+6")]
    #[case::negative_exponent("150000e-2")]
    fn equivalent_notations_share_one_canonical_form(#[case] input: &str) {
        assert_eq!(canonical(input).as_deref(), Some("15e2"));
    }

    #[rstest]
    #[case::zero("0")]
    #[case::negative_zero_with_exponent("-0.000e+99")]
    fn every_zero_is_unsigned(#[case] input: &str) {
        assert_eq!(canonical(input).as_deref(), Some("0"));
    }

    #[test]
    fn distinct_values_stay_distinct() {
        let pairs = [
            ("9007199254740992", "9007199254740993"),
            ("0.1", "0.10000000000000000000000000001"),
            ("-1", "1"),
            ("1e-5", "1e5"),
        ];

        for (left, right) in pairs {
            assert_ne!(canonical(left), canonical(right), "{left} vs {right}");
        }
    }

    #[test]
    fn accepts_exponents_up_to_the_i128_bounds() {
        let cases = [
            (format!("1e{I128_MAX}"), format!("1e{I128_MAX}")),
            (format!("1e{I128_MIN}"), format!("1e{I128_MIN}")),
            (format!("1.0e{I128_MIN}"), format!("1e{I128_MIN}")),
            (format!("0e{I128_MAX}"), "0".to_owned()),
        ];

        for (input, expected) in cases {
            assert_eq!(
                canonical(&input).as_deref(),
                Some(expected.as_str()),
                "{input}"
            );
        }
    }

    #[test]
    fn rejects_values_whose_exponent_leaves_the_i128_range() {
        let inputs = [
            format!("1e{I128_MAX_PLUS_ONE}"),
            format!("1e{I128_MIN_MINUS_ONE}"),
            format!("0e{I128_MAX_PLUS_ONE}"),
            format!("10e{I128_MAX}"),
            format!("0.1e{I128_MIN}"),
        ];

        for input in inputs {
            assert_eq!(canonical(&input), None, "{input}");
        }
    }
}
