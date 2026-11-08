//! Exact finite decimal addition. No binary floats, rounding or expression execution.
use crate::workspace::{Failure, Result};
const PRECISION: usize = 1024;
fn invalid() -> Failure {
    Failure::new(
        "WORKBOOK_NUMBER_INVALID",
        "Ячейка содержит некорректное или неограниченное числовое значение",
    )
}
fn precision() -> Failure {
    Failure::new(
        "WORKBOOK_SUM_PRECISION",
        "Сумму нельзя получить без потери точности в установленном пределе вычисления",
    )
}
#[derive(Clone, Default)]
pub(crate) struct Decimal {
    negative: bool,
    digits: Vec<u8>,
    exponent: i32,
}
impl Decimal {
    fn normalize(&mut self) -> Result<()> {
        while self.digits.last() == Some(&0) {
            self.digits.pop();
        }
        let zeros = self.digits.iter().take_while(|v| **v == 0).count();
        self.digits.drain(..zeros);
        if self.digits.is_empty() {
            self.negative = false;
            self.exponent = 0;
            return Ok(());
        }
        self.exponent = self
            .exponent
            .checked_add(zeros as i32)
            .ok_or_else(precision)?;
        if self.digits.len() > PRECISION
            || self.exponent < -1_001_022
            || self.exponent as i64 + self.digits.len() as i64 - 1 > 999_999
        {
            return Err(precision());
        }
        Ok(())
    }
    pub(crate) fn parse(value: &str) -> Result<Self> {
        if value.is_empty() || value.len() > 4096 {
            return Err(invalid());
        }
        let (negative, value) = if let Some(v) = value.strip_prefix('-') {
            (true, v)
        } else {
            (false, value.strip_prefix('+').unwrap_or(value))
        };
        let mut split = value.split(['e', 'E']);
        let coefficient = split.next().unwrap();
        let power = match split.next() {
            Some(v) => v.parse::<i32>().map_err(|_| invalid())?,
            None => 0,
        };
        if split.next().is_some() || power.abs_diff(0) > 1_005_118 {
            return Err(invalid());
        }
        let mut decimal = false;
        let mut fractional = 0;
        let mut digits = Vec::new();
        for byte in coefficient.bytes() {
            if byte == b'.' && !decimal {
                decimal = true;
                continue;
            }
            if !byte.is_ascii_digit() {
                return Err(invalid());
            }
            digits.push(byte - b'0');
            if decimal {
                fractional += 1;
            }
        }
        if digits.is_empty() {
            return Err(invalid());
        }
        digits.reverse();
        let mut result = Self {
            negative,
            digits,
            exponent: power.checked_sub(fractional).ok_or_else(invalid)?,
        };
        result.normalize()?;
        Ok(result)
    }
    pub(crate) fn add(&mut self, other: &Self) -> Result<()> {
        if other.digits.is_empty() {
            return Ok(());
        }
        if self.digits.is_empty() {
            *self = other.clone();
            return Ok(());
        }
        let exponent = self.exponent.min(other.exponent);
        let left = (self.exponent - exponent) as usize;
        let right = (other.exponent - exponent) as usize;
        let length = (left + self.digits.len()).max(right + other.digits.len());
        // Disjoint coefficients beyond this span cannot cancel into an exact
        // 1024-digit result. Reject rather than allocate by an arbitrary exponent.
        if length > 2 * PRECISION + 2 {
            return Err(precision());
        }
        let mut a = vec![0; length];
        a[left..left + self.digits.len()].copy_from_slice(&self.digits);
        let mut b = vec![0; length];
        b[right..right + other.digits.len()].copy_from_slice(&other.digits);
        let mut negative = self.negative;
        let digits = if self.negative == other.negative {
            let mut carry = 0;
            let mut result = Vec::with_capacity(length + 1);
            for index in 0..length {
                let sum = a[index] + b[index] + carry;
                result.push(sum % 10);
                carry = sum / 10;
            }
            if carry != 0 {
                result.push(carry);
            }
            result
        } else {
            if a.iter().rev().cmp(b.iter().rev()).is_lt() {
                std::mem::swap(&mut a, &mut b);
                negative = other.negative;
            }
            let mut borrow = 0;
            let mut result = Vec::with_capacity(length);
            for index in 0..length {
                let value = a[index] as i16 - b[index] as i16 - borrow;
                result.push(value.rem_euclid(10) as u8);
                borrow = i16::from(value < 0);
            }
            result
        };
        let mut result = Self {
            negative,
            digits,
            exponent,
        };
        result.normalize()?;
        *self = result;
        Ok(())
    }
    pub(crate) fn display(&self) -> String {
        if self.digits.is_empty() {
            return "0".into();
        }
        let digits: String = self
            .digits
            .iter()
            .rev()
            .map(|v| char::from(b'0' + v))
            .collect();
        let adjusted = self.exponent + self.digits.len() as i32 - 1;
        let sign = if self.negative { "-" } else { "" };
        // Match Decimal.__str__ outside the explicit fixed-display interval:
        // a long integer with a fractional tail still uses fixed notation.
        if !(-12..=20).contains(&adjusted) && (self.exponent > 0 || adjusted < -6) {
            let fraction = if digits.len() == 1 {
                String::new()
            } else {
                format!(".{}", &digits[1..])
            };
            return format!("{sign}{}{fraction}E{adjusted:+}", &digits[..1]);
        }
        let point = digits.len() as i32 + self.exponent;
        if point <= 0 {
            format!("{sign}0.{}{digits}", "0".repeat((-point) as usize))
        } else if point >= digits.len() as i32 {
            format!(
                "{sign}{digits}{}",
                "0".repeat(point as usize - digits.len())
            )
        } else {
            format!(
                "{sign}{}.{}",
                &digits[..point as usize],
                &digits[point as usize..]
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finite_values_are_exact_across_cancellation_and_scales() {
        let mut sum = Decimal::default();
        for _ in 0..600 {
            sum.add(&Decimal::parse("0.1").unwrap()).unwrap();
        }
        assert_eq!(sum.display(), "60");
        sum.add(&Decimal::parse("-60.000").unwrap()).unwrap();
        assert_eq!(sum.display(), "0");
        for (a, b, expected) in [
            ("-1.2", ".9", "-0.3"),
            ("1E+200", "-1E+200", "0"),
            ("1.2e-324", ".3e-324", "1.5E-324"),
            ("999", "1", "1000"),
            ("10.010", "-.001", "10.009"),
        ] {
            let mut value = Decimal::parse(a).unwrap();
            value.add(&Decimal::parse(b).unwrap()).unwrap();
            assert_eq!(value.display(), expected);
        }
        assert!(Decimal::parse("NaN").is_err());
        assert!(Decimal::parse("Infinity").is_err());
        let mut value = Decimal::parse("1e999999").unwrap();
        assert!(value.add(&Decimal::parse("1").unwrap()).is_err());
    }
}
