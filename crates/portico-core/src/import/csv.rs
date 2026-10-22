//! Minimal RFC 4180 CSV reader for imports (no dependency).
//!
//! Tolerant on input as spreadsheets write it: `\r\n`, `\n` or `\r` line
//! ends, quoted fields with `""` escapes and line breaks inside quotes,
//! text after a closing quote kept as is, `,` or `;` as delimiter (chosen
//! by the caller), blank lines skipped. Strict where it matters: an
//! unterminated quote is an error (never a silently merged record), and
//! the record and field counts are bounded.
//!
//! Every field is a [`Zeroizing`] string: an export may hold passwords and
//! private keys, which are wiped from memory when the records are dropped.

use zeroize::Zeroizing;

/// Most records read from one file.
pub(crate) const MAX_RECORDS: usize = 5000;
/// Most fields in one record.
pub(crate) const MAX_FIELDS: usize = 64;

/// One CSV record and the line it starts on (1-based).
#[derive(Debug)]
pub(crate) struct Record {
    pub(crate) line: usize,
    pub(crate) fields: Vec<Zeroizing<String>>,
}

/// Why a CSV text could not be read (no field content, ever).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CsvError {
    UnterminatedQuote { line: usize },
    TooManyRecords,
    TooManyFields { line: usize },
}

/// Accumulates records, skipping blank lines and enforcing the bounds.
struct Records {
    out: Vec<Record>,
    fields: Vec<Zeroizing<String>>,
    field: Zeroizing<String>,
}

impl Records {
    fn end_field(&mut self, line: usize) -> Result<(), CsvError> {
        self.fields.push(std::mem::take(&mut self.field));
        if self.fields.len() > MAX_FIELDS {
            return Err(CsvError::TooManyFields { line });
        }
        Ok(())
    }

    fn end_record(&mut self, line: usize) -> Result<(), CsvError> {
        self.end_field(line)?;
        let fields = std::mem::take(&mut self.fields);
        let blank = fields.len() == 1 && fields.first().is_some_and(|f| f.is_empty());
        if blank {
            return Ok(());
        }
        if self.out.len() >= MAX_RECORDS {
            return Err(CsvError::TooManyRecords);
        }
        self.out.push(Record { line, fields });
        Ok(())
    }
}

/// Splits `text` into records. Records made of a single empty field
/// (blank lines) are skipped.
pub(crate) fn parse(text: &str, delimiter: char) -> Result<Vec<Record>, CsvError> {
    let mut acc = Records {
        out: Vec::new(),
        fields: Vec::new(),
        field: Zeroizing::new(String::new()),
    };
    let mut line = 1usize;
    let mut record_line = 1usize;
    let mut in_quotes = false;
    let mut quote_line = 1usize;
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        if in_quotes {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    acc.field.push('"');
                }
                '"' => in_quotes = false,
                '\r' | '\n' => {
                    // `\r\n` inside quotes is one line break.
                    if c == '\r' && chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    line = line.saturating_add(1);
                    acc.field.push('\n');
                }
                other => acc.field.push(other),
            }
            continue;
        }
        match c {
            '"' if acc.field.is_empty() => {
                in_quotes = true;
                quote_line = line;
            }
            '\r' | '\n' => {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                acc.end_record(record_line)?;
                line = line.saturating_add(1);
                record_line = line;
            }
            c if c == delimiter => acc.end_field(record_line)?,
            other => acc.field.push(other),
        }
    }
    if in_quotes {
        return Err(CsvError::UnterminatedQuote { line: quote_line });
    }
    if !acc.fields.is_empty() || !acc.field.is_empty() {
        acc.end_record(record_line)?;
    }
    Ok(acc.out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(text: &str) -> Vec<(usize, Vec<String>)> {
        parse(text, ',')
            .expect("valid csv")
            .into_iter()
            .map(|r| (r.line, r.fields.iter().map(|f| f.to_string()).collect()))
            .collect()
    }

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn plain_quoted_and_escaped_fields() {
        assert_eq!(
            rows("a,b,c\r\n1,\"x,y\",\"say \"\"hi\"\"\"\r\n"),
            vec![
                (1, strings(&["a", "b", "c"])),
                (2, strings(&["1", "x,y", "say \"hi\""])),
            ]
        );
    }

    #[test]
    fn line_breaks_inside_quotes_and_line_numbers() {
        assert_eq!(
            rows("h\n\"one\r\ntwo\",x\n\nlast"),
            vec![
                (1, strings(&["h"])),
                (2, strings(&["one\ntwo", "x"])),
                (5, strings(&["last"])),
            ]
        );
        assert_eq!(
            rows("a\rb\r"),
            vec![(1, strings(&["a"])), (2, strings(&["b"]))]
        );
    }

    #[test]
    fn empty_fields_and_trailing_delimiter() {
        assert_eq!(
            rows(",,\nx,"),
            vec![(1, strings(&["", "", ""])), (2, strings(&["x", ""]))]
        );
    }

    #[test]
    fn lenient_text_after_a_closing_quote() {
        assert_eq!(rows("\"ab\"c,d"), vec![(1, strings(&["abc", "d"]))]);
        // A quote in the middle of an unquoted field is literal.
        assert_eq!(rows("a\"b,c"), vec![(1, strings(&["a\"b", "c"]))]);
    }

    #[test]
    fn semicolon_delimiter() {
        let records = parse("a;b,c\n", ';').expect("valid");
        let fields: Vec<String> = records[0].fields.iter().map(|f| f.to_string()).collect();
        assert_eq!(fields, ["a", "b,c"]);
    }

    #[test]
    fn errors() {
        assert_eq!(
            parse("a,b\nc,\"open\nstill open", ',').map(|_| ()),
            Err(CsvError::UnterminatedQuote { line: 2 })
        );
        let wide = ",".repeat(MAX_FIELDS);
        assert_eq!(
            parse(&format!("ok\n{wide}"), ',').map(|_| ()),
            Err(CsvError::TooManyFields { line: 2 })
        );
        let max_wide = ",".repeat(MAX_FIELDS - 1);
        assert!(parse(&max_wide, ',').is_ok());
        let many = "x\n".repeat(MAX_RECORDS + 1);
        assert_eq!(parse(&many, ',').map(|_| ()), Err(CsvError::TooManyRecords));
        assert_eq!(
            parse(&"x\n".repeat(MAX_RECORDS), ',').map(|r| r.len()),
            Ok(MAX_RECORDS)
        );
    }
}
