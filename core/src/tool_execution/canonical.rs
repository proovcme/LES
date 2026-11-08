//! Compatibility with existing Python JSON fingerprints, including float exponent spelling.
use serde_json::Value;
fn float(number: f64) -> Result<String, &'static str> {
    if !number.is_finite() {
        return Err("TOOL_ARGUMENTS_NOT_JSON");
    }
    let sign = if number.is_sign_negative() { "-" } else { "" };
    if number == 0. {
        return Ok(format!("{sign}0.0"));
    }
    let raw = serde_json::to_string(&number.abs()).map_err(|_| "TOOL_ARGUMENTS_NOT_JSON")?;
    let (mantissa, exponent) = if let Some((a, b)) = raw.split_once('e') {
        (a, b.parse::<i32>().map_err(|_| "TOOL_ARGUMENTS_NOT_JSON")?)
    } else {
        (raw.as_str(), 0)
    };
    let point = mantissa.find('.').unwrap_or(mantissa.len()) as i32;
    let all = mantissa.replace('.', "");
    let first = all
        .bytes()
        .position(|v| v != b'0')
        .ok_or("TOOL_ARGUMENTS_NOT_JSON")?;
    let exponent = exponent + point - first as i32 - 1;
    let digits = all[first..].trim_end_matches('0');
    if (-4..16).contains(&exponent) {
        let point = exponent + 1;
        let fixed = if point <= 0 {
            format!("0.{}{}", "0".repeat((-point) as usize), digits)
        } else if point as usize >= digits.len() {
            format!("{}{}.0", digits, "0".repeat(point as usize - digits.len()))
        } else {
            format!(
                "{}.{}",
                &digits[..point as usize],
                &digits[point as usize..]
            )
        };
        Ok(format!("{sign}{fixed}"))
    } else {
        let mantissa = if digits.len() == 1 {
            digits.to_string()
        } else {
            format!("{}.{}", &digits[..1], &digits[1..])
        };
        Ok(format!(
            "{sign}{mantissa}e{}{value:02}",
            if exponent < 0 { '-' } else { '+' },
            value = exponent.abs()
        ))
    }
}
pub fn encode(value: &Value) -> Result<String, &'static str> {
    match value {
        Value::Object(map) => {
            let mut values = map.iter().collect::<Vec<_>>();
            values.sort_by_key(|v| v.0);
            Ok(format!(
                "{{{}}}",
                values
                    .iter()
                    .map(|(k, v)| Ok(format!(
                        "{}:{}",
                        serde_json::to_string(k).map_err(|_| "TOOL_ARGUMENTS_NOT_JSON")?,
                        encode(v)?
                    )))
                    .collect::<Result<Vec<_>, &'static str>>()?
                    .join(",")
            ))
        }
        Value::Array(values) => Ok(format!(
            "[{}]",
            values
                .iter()
                .map(encode)
                .collect::<Result<Vec<_>, _>>()?
                .join(",")
        )),
        Value::Number(value) => {
            let raw = value.to_string();
            if raw.contains(['.', 'e', 'E']) {
                let number = raw.parse::<f64>().map_err(|_| "TOOL_ARGUMENTS_NOT_JSON")?;
                float(number)
            } else {
                Ok(if raw == "-0" { "0".into() } else { raw })
            }
        }
        _ => serde_json::to_string(value).map_err(|_| "TOOL_ARGUMENTS_NOT_JSON"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn float_ties_notation_and_negative_zero_preserve_existing_fingerprints() {
        for (number, expected) in [
            (-2033196898701425.25, "-2033196898701425.2"),
            (1e-5, "1e-05"),
            (1e-4, "0.0001"),
            (1e16, "1e+16"),
            (-0.0, "-0.0"),
            (f64::from_bits(1), "5e-324"),
        ] {
            assert_eq!(float(number).unwrap(), expected);
        }
    }
}
