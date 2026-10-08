//! A bounded arithmetic parser for the calculate chat tool.

pub fn evaluate(expression: &str) -> Result<f64, String> {
    if expression.is_empty() || expression.len() > 200 {
        return Err("Expression must be 1 through 200 characters".into());
    }
    let mut parser = Parser {
        input: expression.as_bytes(),
        position: 0,
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
        if self.consume(b'(') {
            let value = self.expression()?;
            if !self.consume(b')') {
                return Err("Missing closing parenthesis".into());
            }
            return Ok(value);
        }
        self.number()
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

#[cfg(test)]
mod tests {
    use super::evaluate;

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
}
