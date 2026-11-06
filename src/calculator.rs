//! A bounded arithmetic parser for the calculate and plot chat tools.

pub fn evaluate(expression: &str) -> Result<f64, String> {
    evaluate_with(expression, None)
}

pub fn evaluate_at(expression: &str, x: f64) -> Result<f64, String> {
    if !x.is_finite() {
        return Err("Expression variable is not a finite number".into());
    }
    evaluate_with(expression, Some(x))
}

fn evaluate_with(expression: &str, variable: Option<f64>) -> Result<f64, String> {
    if expression.is_empty() || expression.len() > 200 {
        return Err("Expression must be 1 through 200 characters".into());
    }
    let mut parser = Parser {
        input: expression.as_bytes(),
        position: 0,
        variable,
    };
    let value = parser.expression()?;
    parser.skip_spaces();
    if parser.position != parser.input.len() {
        return Err("Invalid calculation expression".into());
    }
    Ok(value)
}

struct Parser<'a> {
    input: &'a [u8],
    position: usize,
    variable: Option<f64>,
}

impl Parser<'_> {
    fn skip_spaces(&mut self) {
        while self
            .input
            .get(self.position)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.position += 1;
        }
    }

    fn consume(&mut self, expected: u8) -> bool {
        self.skip_spaces();
        if self.input.get(self.position) == Some(&expected) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn expression(&mut self) -> Result<f64, String> {
        let mut value = self.term()?;
        loop {
            if self.consume(b'+') {
                value += self.term()?;
            } else if self.consume(b'-') {
                value -= self.term()?;
            } else {
                return Ok(value);
            }
            ensure_finite(value)?;
        }
    }

    fn term(&mut self) -> Result<f64, String> {
        let mut value = self.factor()?;
        loop {
            if self.consume(b'*') {
                value *= self.factor()?;
            } else if self.consume(b'/') {
                let divisor = self.factor()?;
                if divisor == 0.0 {
                    return Err("Cannot divide by zero".into());
                }
                value /= divisor;
            } else {
                return Ok(value);
            }
            ensure_finite(value)?;
        }
    }

    fn factor(&mut self) -> Result<f64, String> {
        if self.consume(b'+') {
            return self.factor();
        }
        if self.consume(b'-') {
            return Ok(-self.factor()?);
        }
        let mut value = self.atom()?;
        while self.consume(b'!') {
            value = factorial(value)?;
        }
        Ok(value)
    }

    fn atom(&mut self) -> Result<f64, String> {
        if self.consume(b'(') {
            let value = self.expression()?;
            if !self.consume(b')') {
                return Err("Missing closing parenthesis".into());
            }
            return Ok(value);
        }
        if let Some(value) = self.bound_variable()? {
            return Ok(value);
        }
        self.number()
    }

    fn bound_variable(&mut self) -> Result<Option<f64>, String> {
        self.skip_spaces();
        if self.input.get(self.position) != Some(&b'x') {
            return Ok(None);
        }
        let next = self.input.get(self.position + 1).copied();
        if next.is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_') {
            return Ok(None);
        }
        self.position += 1;
        match self.variable {
            Some(value) => Ok(Some(value)),
            None => Err("Expression uses x but no value was supplied".into()),
        }
    }

    fn number(&mut self) -> Result<f64, String> {
        self.skip_spaces();
        let start = self.position;
        self.digits();
        if self.input.get(self.position) == Some(&b'.') {
            self.position += 1;
            self.digits();
        }
        if self.position == start || self.input[start..self.position] == *b"." {
            return Err("Expected a number".into());
        }
        if matches!(self.input.get(self.position), Some(b'e' | b'E')) {
            self.position += 1;
            if matches!(self.input.get(self.position), Some(b'+' | b'-')) {
                self.position += 1;
            }
            let exponent_start = self.position;
            self.digits();
            if self.position == exponent_start {
                return Err("Invalid number exponent".into());
            }
        }
        let text = std::str::from_utf8(&self.input[start..self.position])
            .map_err(|_| "Invalid calculation expression")?;
        let value = text.parse::<f64>().map_err(|_| "Invalid number")?;
        ensure_finite(value)?;
        Ok(value)
    }

    fn digits(&mut self) {
        while self
            .input
            .get(self.position)
            .is_some_and(u8::is_ascii_digit)
        {
            self.position += 1;
        }
    }
}

fn ensure_finite(value: f64) -> Result<(), String> {
    if value.is_finite() {
        Ok(())
    } else {
        Err("Calculation result is too large".into())
    }
}

fn factorial(value: f64) -> Result<f64, String> {
    if !value.is_finite() {
        return Err("Calculation result is too large".into());
    }
    if is_near_integer(value) {
        let n = value.round();
        if n < 0.0 {
            return Err("Factorial is not defined for negative integers".into());
        }
        if n > 170.0 {
            return Err("Calculation result is too large".into());
        }
        let mut result = 1.0;
        for factor in 2..=n as u32 {
            result *= f64::from(factor);
        }
        ensure_finite(result)?;
        return Ok(result);
    }
    // x! = Γ(x + 1), so a plot can pass through fractional samples.
    gamma(value + 1.0)
}

fn is_near_integer(value: f64) -> bool {
    (value - value.round()).abs() <= 1e-9 * (1.0 + value.abs())
}

fn gamma(z: f64) -> Result<f64, String> {
    if !z.is_finite() {
        return Err("Calculation result is too large".into());
    }
    if z < 0.5 {
        let sine = (std::f64::consts::PI * z).sin();
        if sine.abs() <= 1e-12 {
            return Err("Factorial is not defined for negative integers".into());
        }
        let value = std::f64::consts::PI / (sine * gamma(1.0 - z)?);
        ensure_finite(value)?;
        return Ok(value);
    }
    lanczos_gamma(z)
}

/// Lanczos approximation for Γ(z), valid on the path where z >= 0.5.
fn lanczos_gamma(mut z: f64) -> Result<f64, String> {
    const G: f64 = 7.0;
    const COEFFICIENTS: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    z -= 1.0;
    let mut acc = COEFFICIENTS[0];
    for (index, coefficient) in COEFFICIENTS.iter().enumerate().skip(1) {
        acc += coefficient / (z + index as f64);
    }
    let t = z + G + 0.5;
    let value = (2.0 * std::f64::consts::PI).sqrt() * t.powf(z + 0.5) * (-t).exp() * acc;
    ensure_finite(value)?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::{evaluate, evaluate_at};

    #[test]
    fn observes_precedence_parentheses_and_scientific_numbers() {
        assert_eq!(evaluate("2 + 3 * 4").unwrap(), 14.0);
        assert_eq!(evaluate("(2 + 3) * -4").unwrap(), -20.0);
        assert_eq!(evaluate("1.2e2 / .5").unwrap(), 240.0);
    }

    #[test]
    fn rejects_bad_expressions_and_nonfinite_results() {
        for expression in ["", "2 +", "1e+", "(2 + 3", "1 + 2 garbage", "1e309"] {
            assert!(evaluate(expression).is_err(), "{expression}");
        }
        assert_eq!(
            evaluate("3 / (2 - 2)").unwrap_err(),
            "Cannot divide by zero"
        );
    }

    #[test]
    fn evaluates_x_only_when_a_value_is_supplied() {
        assert_eq!(evaluate_at("(x + 1) * 2", 3.0).unwrap(), 8.0);
        assert_eq!(evaluate_at("-x", 2.5).unwrap(), -2.5);
        assert_eq!(
            evaluate("x + 1").unwrap_err(),
            "Expression uses x but no value was supplied"
        );
        assert!(evaluate_at("2x", 1.0).is_err());
        assert!(evaluate_at("x", f64::INFINITY).is_err());
    }

    #[test]
    fn factorial_is_postfix_and_rejects_negative_integers() {
        assert_eq!(evaluate("5!").unwrap(), 120.0);
        assert_eq!(evaluate("0!").unwrap(), 1.0);
        assert_eq!(evaluate("2 * 3!").unwrap(), 12.0);
        assert_eq!(evaluate("-3!").unwrap(), -6.0);
        assert_eq!(evaluate("(2 + 1)!").unwrap(), 6.0);
        assert_eq!(evaluate_at("x!", 6.0).unwrap(), 720.0);
        assert_eq!(evaluate_at("(x + 1)!", 3.0).unwrap(), 24.0);
        let half = evaluate("0.5!").unwrap();
        assert!((half - 0.886_226_925_45).abs() < 1e-8, "{half}");
        assert_eq!(
            evaluate("(-3)!").unwrap_err(),
            "Factorial is not defined for negative integers"
        );
        assert_eq!(
            evaluate_at("x!", -1.0).unwrap_err(),
            "Factorial is not defined for negative integers"
        );
        assert_eq!(
            evaluate("200!").unwrap_err(),
            "Calculation result is too large"
        );
    }
}
