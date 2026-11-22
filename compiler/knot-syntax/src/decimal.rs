//! Exact decimal input. A token denotes `significand * 10^exponent`; both
//! parts are unbounded integers. Constructing one never expands the power of
//! ten or rounds to a machine float. Unary minus remains an AST operation.

use num_bigint::{BigInt, Sign};
use std::str::FromStr;

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DecimalLiteral {
    significand: BigInt,
    exponent: BigInt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidDecimal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecimalExpansionLimit;

impl std::fmt::Display for InvalidDecimal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid nonnegative decimal literal")
    }
}

impl std::error::Error for InvalidDecimal {}

impl std::fmt::Display for DecimalExpansionLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("decimal exponent exceeds the rational expansion limit")
    }
}

impl std::error::Error for DecimalExpansionLimit {}

impl DecimalLiteral {
    pub fn significand(&self) -> &BigInt {
        &self.significand
    }
    pub fn exponent(&self) -> &BigInt {
        &self.exponent
    }

    /// Reduced numerator and positive denominator for `Ratio Integer` input.
    /// The caller supplies an expansion limit: a short token such as `1e99999999`
    /// must not allocate an enormous integer just because it parsed successfully.
    pub fn rational_parts(
        &self,
        max_power: u32,
    ) -> Result<(BigInt, BigInt), DecimalExpansionLimit> {
        let digits = self.exponent.magnitude().to_u32_digits();
        let power = match digits.as_slice() {
            [] => 0,
            [power] if *power <= max_power => *power,
            _ => return Err(DecimalExpansionLimit),
        };
        let factor = BigInt::from(10).pow(power);
        if self.exponent.sign() != Sign::Minus {
            return Ok((&self.significand * factor, BigInt::from(1)));
        }
        let mut a = self.significand.clone();
        let mut b = factor.clone();
        while b != BigInt::from(0) {
            let remainder = a % &b;
            a = b;
            b = remainder;
        }
        Ok((&self.significand / &a, factor / a))
    }

    /// Round only when the selected numeric instance is Float. Very large or
    /// small finite decimal inputs may become infinity or zero at this point.
    pub fn to_f64(&self) -> f64 {
        format!("{}e{}", self.significand, self.exponent)
            .parse()
            .expect("validated decimal scientific notation")
    }
}

impl FromStr for DecimalLiteral {
    type Err = InvalidDecimal;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (mantissa, exponent) = match text.find(['e', 'E']) {
            Some(index) => {
                let exponent = &text[index + 1..];
                let digits = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
                if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(InvalidDecimal);
                }
                (
                    &text[..index],
                    exponent.parse::<BigInt>().map_err(|_| InvalidDecimal)?,
                )
            }
            None => (text, BigInt::from(0)),
        };
        let (whole, fractional) = match mantissa.split_once('.') {
            Some((whole, fraction)) if !fraction.is_empty() => (whole, fraction),
            Some(_) => return Err(InvalidDecimal),
            None => (mantissa, ""),
        };
        if whole.is_empty()
            || !whole
                .bytes()
                .chain(fractional.bytes())
                .all(|b| b.is_ascii_digit())
        {
            return Err(InvalidDecimal);
        }
        let digits = format!("{whole}{fractional}");
        let significant = digits.trim_end_matches('0');
        if significant.is_empty() {
            return Ok(Self {
                significand: BigInt::from(0),
                exponent: BigInt::from(0),
            });
        }
        Ok(Self {
            significand: significant.parse().map_err(|_| InvalidDecimal)?,
            exponent: exponent - BigInt::from(fractional.len())
                + BigInt::from(digits.len() - significant.len()),
        })
    }
}

impl std::fmt::Display for DecimalLiteral {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Always decimal/exponent syntax, even for integral values, so printing
        // a literal does not change its Fractional constraint into Num.
        write!(f, "{}e{}", self.significand, self.exponent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_rationals_are_exact_and_reduced() {
        for (text, numerator, denominator) in [
            ("0.1", 1, 10),
            ("2.500", 5, 2),
            ("15e-1", 3, 2),
            ("1.25e2", 125, 1),
            ("0e-999999999999999999999", 0, 1),
        ] {
            let decimal: DecimalLiteral = text.parse().unwrap();
            assert_eq!(
                decimal.rational_parts(100).unwrap(),
                (numerator.into(), denominator.into())
            );
            assert_eq!(
                decimal.to_string().parse::<DecimalLiteral>().unwrap(),
                decimal
            );
        }
    }

    #[test]
    fn precision_and_exponents_are_not_limited_by_f64_or_machine_integers() {
        let precise: DecimalLiteral = "0.10000000000000000000000000000000000001".parse().unwrap();
        assert_eq!(
            precise.significand().to_string(),
            "10000000000000000000000000000000000001"
        );
        for text in ["1e999999999999999999999999", "1e-999999999999999999999999"] {
            let decimal: DecimalLiteral = text.parse().unwrap();
            assert_eq!(decimal.significand(), &BigInt::from(1));
            assert!(decimal.rational_parts(100).is_err());
            assert_eq!(decimal.to_string(), text);
        }
        assert!("1e999999999999999999999999"
            .parse::<DecimalLiteral>()
            .unwrap()
            .to_f64()
            .is_infinite());
        assert_eq!(
            "1e-999999999999999999999999"
                .parse::<DecimalLiteral>()
                .unwrap()
                .to_f64(),
            0.0
        );
    }

    #[test]
    fn validation_and_expansion_limits_are_explicit() {
        for text in [
            "", ".5", "5.", "-1.0", "1e", "1e+", "1.2.3", "1e2e3", "NaN", "Infinity",
        ] {
            assert!(text.parse::<DecimalLiteral>().is_err(), "{text}");
        }
        let decimal: DecimalLiteral = "1e3".parse().unwrap();
        assert!(decimal.rational_parts(2).is_err());
        assert_eq!(decimal.rational_parts(3).unwrap(), (1000.into(), 1.into()));
    }
}
