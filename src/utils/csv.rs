//! A minimal CSV writer (RFC 4180) for staff exports.

/// One CSV field: quoted when it holds a separator, a quote or a line
/// break, and defused when a spreadsheet would run it as a formula
/// (`=`, `+`, `-`, `@`, tab or carriage return starting a cell: CSV
/// injection). A cell can also start after a `;`, a tab or a line break
/// inside the value: spreadsheets in locales whose list separator is `;`
/// (pt-BR, es) split there whatever the quoting, so `x;=1+1` would run
/// too.
pub fn field(value: &str) -> String {
    let mut defused = String::with_capacity(value.len() + 1);
    let mut cell_start = true;
    for c in value.chars() {
        if cell_start && matches!(c, '=' | '+' | '-' | '@' | '\t' | '\r') {
            defused.push('\'');
        }
        defused.push(c);
        cell_start = matches!(c, ';' | '\t' | '\n' | '\r');
    }
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
        // Where a `;`-separated spreadsheet would start a cell.
        assert_eq!(field("x;=1+1;y"), "x;'=1+1;y");
        assert_eq!(field("a\t@cmd"), "a\t'@cmd");
        assert_eq!(field("a; b"), "a; b");
        assert_eq!(field("a\n=b"), "\"a\n'=b\"");
        let mut out = String::new();
        push_row(&mut out, ["a", "b,c"]);
        assert_eq!(out, "a,\"b,c\"\r\n");
    }
}
