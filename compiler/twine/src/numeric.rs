//! Binary64 conversion and rendering. Conversion rounds exact integer ratios
//! once, ties to even. Rendering uses open rounding intervals, as GHC's Show
//! does, rather than assuming Rust's choice of shortest digits is identical.
//! Rendering reference: GHC 9.6.2, libraries/base/GHC/Float.hs, floatToDigits.
//! The checked-in GHC generator supplies an independent rendering oracle.
use crate::machine::RuntimeErrorKind;
use num_bigint::{BigInt, Sign};

fn small(n: &BigInt) -> u64 {
    n.magnitude().iter_u64_digits().next().unwrap_or(0)
}
fn magnitude(n: &BigInt) -> BigInt {
    BigInt::from(n.magnitude().clone())
}

pub(crate) fn ratio_to_float(n: &BigInt, d: &BigInt) -> Result<f64, RuntimeErrorKind> {
    if d.sign() == Sign::NoSign {
        return Err(RuntimeErrorKind::DivideByZero);
    }
    let negative = (n.sign() == Sign::Minus) != (d.sign() == Sign::Minus);
    // Both operands convert exactly. Binary64 division then supplies the one
    // rounding required by Ratio, including signed zero for a negative divisor.
    if n.bits() <= 53 && d.bits() <= 53 {
        let quotient = small(n) as f64 / small(d) as f64;
        return Ok(if negative { -quotient } else { quotient });
    }
    let sign = if negative { 1u64 << 63 } else { 0 };
    let n = magnitude(n);
    let d = magnitude(d);
    if n.sign() == Sign::NoSign {
        return Ok(f64::from_bits(sign));
    }
    // The bit-length estimate differs from floor(log2(n/d)) by at most one.
    let mut exponent = n.bits() as i64 - d.bits() as i64;
    if exponent > 1024 {
        return Ok(f64::from_bits(sign | 0x7ff0_0000_0000_0000));
    }
    if exponent < -1075 {
        return Ok(f64::from_bits(sign));
    }
    let below = if exponent >= 0 {
        n < (&d << exponent as usize)
    } else {
        (&n << (-exponent) as usize) < d
    };
    if below {
        exponent -= 1;
    }
    if exponent > 1023 {
        return Ok(f64::from_bits(sign | 0x7ff0_0000_0000_0000));
    }
    // Subnormals share a fixed 2^-1074 unit; normals keep 53 significant bits.
    let shift = 52 - exponent.max(-1022);
    let (scaled_n, scaled_d) = if shift >= 0 {
        (n << shift as usize, d)
    } else {
        (n, d << (-shift) as usize)
    };
    let mut q = &scaled_n / &scaled_d;
    let remainder = &scaled_n % &scaled_d;
    let twice = &remainder << 1usize;
    if twice > scaled_d || (twice == scaled_d && q.bit(0)) {
        q += 1;
    }
    let mut significand = small(&q);
    if exponent < -1022 {
        return Ok(f64::from_bits(sign | significand));
    }
    if significand == 1u64 << 53 {
        significand >>= 1;
        exponent += 1;
    }
    if exponent > 1023 {
        return Ok(f64::from_bits(sign | 0x7ff0_0000_0000_0000));
    }
    Ok(f64::from_bits(
        sign | (((exponent + 1023) as u64) << 52) | (significand - (1u64 << 52)),
    ))
}
fn decode(value: f64) -> (u64, i32) {
    let bits = value.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1u64 << 52) - 1);
    if exp == 0 {
        (fraction, -1074)
    } else {
        (fraction | (1u64 << 52), exp - 1023 - 52)
    }
}
pub(crate) fn float_to_ratio(value: f64) -> Result<(BigInt, BigInt), RuntimeErrorKind> {
    if !value.is_finite() {
        return Err(RuntimeErrorKind::InvalidNumeric(
            "non-finite Float to rational".into(),
        ));
    }
    let (mut mantissa, mut exponent) = decode(value);
    if mantissa == 0 {
        return Ok((0.into(), 1.into()));
    }
    let zeros = mantissa.trailing_zeros().min((-exponent).max(0) as u32);
    mantissa >>= zeros;
    exponent += zeros as i32;
    let mut n = BigInt::from(mantissa);
    let mut d = BigInt::from(1);
    if exponent >= 0 {
        n <<= exponent as usize;
    } else {
        d <<= (-exponent) as usize;
    }
    if value.is_sign_negative() {
        n = -n;
    }
    Ok((n, d))
}
pub(crate) fn truncate(value: f64) -> Result<BigInt, RuntimeErrorKind> {
    if !value.is_finite() {
        return Err(RuntimeErrorKind::InvalidNumeric(
            "non-finite Float to Integer".into(),
        ));
    }
    let (mantissa, exponent) = decode(value);
    let mut n = BigInt::from(mantissa);
    if exponent >= 0 {
        n <<= exponent as usize;
    } else {
        n >>= (-exponent) as usize;
    }
    if value.is_sign_negative() {
        n = -n;
    }
    Ok(n)
}

/// Decimal digits within *open* binary rounding boundaries. Excluding endpoints
/// deliberately matches GHC's floatToDigits, including the 1e23 boundary case.
fn digits(value: f64) -> (String, i32) {
    let (mantissa, binary_exponent) = decode(value);
    let mut numerator = BigInt::from(mantissa) << 2usize;
    let mut denominator = BigInt::from(4);
    let mut upper = BigInt::from(2);
    let mut lower = BigInt::from(if mantissa == 1u64 << 52 && binary_exponent > -1074 {
        1
    } else {
        2
    });
    if binary_exponent >= 0 {
        numerator <<= binary_exponent as usize;
        upper <<= binary_exponent as usize;
        lower <<= binary_exponent as usize;
    } else {
        denominator <<= (-binary_exponent) as usize;
    }
    let bound = &numerator + &upper;
    let within = |exponent: i32| {
        if exponent >= 0 {
            bound <= &denominator * BigInt::from(10).pow(exponent as u32)
        } else {
            &bound * BigInt::from(10).pow((-exponent) as u32) <= denominator
        }
    };
    let mut exponent = value.log10().floor() as i32 + 1;
    while !within(exponent) {
        exponent += 1;
    }
    while within(exponent - 1) {
        exponent -= 1;
    }
    if exponent >= 0 {
        denominator *= BigInt::from(10).pow(exponent as u32);
    } else {
        let power = BigInt::from(10).pow((-exponent) as u32);
        numerator *= &power;
        upper *= &power;
        lower *= power;
    }
    let mut output = String::new();
    loop {
        numerator *= 10;
        upper *= 10;
        lower *= 10;
        let digit = small(&(&numerator / &denominator)) as u8;
        numerator %= &denominator;
        let down = numerator < lower;
        let up = &numerator + &upper > denominator;
        if down || up {
            let round = up && (!down || (&numerator << 1usize) >= denominator);
            output.push(char::from(b'0' + digit + u8::from(round)));
            break;
        }
        output.push(char::from(b'0' + digit));
    }
    (output, exponent)
}
pub(crate) fn show(value: f64) -> String {
    if value.is_nan() {
        return "NaN".into();
    }
    let sign = if value.is_sign_negative() { "-" } else { "" };
    if value.is_infinite() {
        return format!("{sign}Infinity");
    }
    if value == 0.0 {
        return format!("{sign}0.0");
    }
    let (digits, exponent) = digits(value.abs());
    if !(0..=7).contains(&exponent) {
        format!(
            "{sign}{}.{}e{}",
            &digits[..1],
            if digits.len() == 1 { "0" } else { &digits[1..] },
            exponent - 1
        )
    } else if exponent == 0 {
        format!("{sign}0.{digits}")
    } else if exponent as usize >= digits.len() {
        format!(
            "{sign}{digits}{}.0",
            "0".repeat(exponent as usize - digits.len())
        )
    } else {
        let (left, right) = digits.split_at(exponent as usize);
        format!("{sign}{left}.{right}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn small_ratios_match_the_exact_conversion_bit_for_bit() {
        for numerator in [0, 1, -7, (1i64 << 53) - 1, 1i64 << 53] {
            for denominator in [-3, 13, (1i64 << 53) - 3] {
                let (n, d) = (BigInt::from(numerator), BigInt::from(denominator));
                let scaled = ratio_to_float(&(&n << 80), &(&d << 80)).unwrap();
                assert_eq!(ratio_to_float(&n, &d).unwrap().to_bits(), scaled.to_bits());
            }
        }
    }
    #[test]
    fn rendering_matches_ghc_9_6_2_goldens() {
        for line in include_str!("../tests/fixtures/float-show.tsv").lines() {
            let (bits, expected) = line.split_once('\t').unwrap();
            let value = f64::from_bits(u64::from_str_radix(bits, 16).unwrap());
            assert_eq!(show(value), expected, "{bits}");
        }
    }
    #[test]
    fn exact_ratios_round_trip_binary64() {
        let mut bits = 0x3141592653589793u64;
        for _ in 0..10_000 {
            bits = bits
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let value = f64::from_bits(bits);
            if value.is_finite() && value != 0.0 {
                let (n, d) = float_to_ratio(value).unwrap();
                assert_eq!(ratio_to_float(&n, &d).unwrap().to_bits(), bits, "{bits:x}");
                assert_eq!(
                    show(value).parse::<f64>().unwrap().to_bits(),
                    bits,
                    "{bits:x}"
                );
            }
        }
    }
    #[test]
    fn ratio_rounding_boundaries() {
        let p = |n| BigInt::from(1) << n;
        // Halfway to the next float above 1: ties to the even significand.
        assert_eq!(ratio_to_float(&(p(53) + 1), &p(53)).unwrap(), 1.0);
        assert_eq!(
            ratio_to_float(&(p(53) + 3), &p(53)).unwrap().to_bits(),
            1.0f64.to_bits() + 2
        );
        // Half a subnormal unit rounds to zero; one-and-a-half rounds to two.
        assert_eq!(ratio_to_float(&1.into(), &p(1075)).unwrap().to_bits(), 0);
        assert_eq!(
            ratio_to_float(&(-1).into(), &p(1075)).unwrap().to_bits(),
            1 << 63
        );
        assert_eq!(ratio_to_float(&3.into(), &p(1075)).unwrap().to_bits(), 2);
        assert_eq!(
            ratio_to_float(&(p(53) - 1), &p(1075)).unwrap().to_bits(),
            1 << 52
        );
        // Finite/infinite midpoint, and the immediately smaller exact integer.
        let midpoint = p(1024) - p(970);
        assert!(ratio_to_float(&midpoint, &1.into()).unwrap().is_infinite());
        assert_eq!(
            ratio_to_float(&(midpoint - 1), &1.into()).unwrap(),
            f64::MAX
        );
        // Neither operand can be separately converted without overflow.
        assert_eq!(ratio_to_float(&(p(4096) * 3), &(p(4096) * 2)).unwrap(), 1.5);
        assert_eq!(ratio_to_float(&3.into(), &(-2).into()).unwrap(), -1.5);
        assert_eq!(
            ratio_to_float(&1.into(), &0.into()),
            Err(RuntimeErrorKind::DivideByZero)
        );
    }
    #[test]
    fn finite_conversions_and_invalid_inputs() {
        assert_eq!(
            float_to_ratio(0.1).unwrap(),
            (3602879701896397u64.into(), 36028797018963968u64.into())
        );
        assert_eq!(float_to_ratio(-0.0).unwrap(), (0.into(), 1.into()));
        assert_eq!(truncate(-1.9).unwrap(), (-1).into());
        assert_eq!(
            truncate(f64::MAX).unwrap(),
            (BigInt::from(1) << 1024usize) - (BigInt::from(1) << 971usize)
        );
        for x in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
            assert!(matches!(
                float_to_ratio(x),
                Err(RuntimeErrorKind::InvalidNumeric(_))
            ));
            assert!(matches!(
                truncate(x),
                Err(RuntimeErrorKind::InvalidNumeric(_))
            ));
        }
    }
}
