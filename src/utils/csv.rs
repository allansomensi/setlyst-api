//! A minimal CSV writer (RFC 4180) for staff exports.

/// One CSV field: quoted when it holds a separator, a quote or a line
/// break, and defused when a spreadsheet would run it as a formula
/// (`=`, `+`, `-`, `@`, tab or carriage return first: CSV injection).
pub fn field(value: &str) -> String {
    let defused = match value.chars().next() {
        Some('=' | '+' | '-' | '@' | '\t' | '\r') => format!("'{value}"),
        _ => value.to_string(),
    };
    if defused.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", defused.replace('"', "\"\""))
    } else {
        defused
    }
}

/// Appends one record (fields joined with commas, CRLF-terminated).
pub fn push_row<I, S>(out: &mut String, fields: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut first = true;
    for value in fields {
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str(&field(value.as_ref()));
    }
    out.push_str("\r\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_are_quoted_and_formulas_defused() {
        assert_eq!(field("plain"), "plain");
        assert_eq!(field("a,b"), "\"a,b\"");
        assert_eq!(field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(field("=SUM(A1)"), "'=SUM(A1)");
        assert_eq!(field("-1"), "'-1");
        let mut out = String::new();
        push_row(&mut out, ["a", "b,c"]);
        assert_eq!(out, "a,\"b,c\"\r\n");
    }
}
