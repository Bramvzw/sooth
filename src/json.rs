//! JSON string escaping, shared by the history file and the machine report —
//! hand-rolled like the rest of sooth's JSON (see `DECISIONS.md`).

use std::fmt::Write as _;

/// `value` as the inside of a JSON string literal.
pub fn escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            control if control.is_control() => {
                // `escaped` is a plain `String`; `write!` never fails for it.
                let _ = write!(escaped, "\\u{:04x}", control as u32);
            }
            other => escaped.push(other),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::escape;

    #[test]
    fn escape_handles_quotes_backslashes_and_control_characters() {
        assert_eq!(
            escape(r#"quote " backslash \ "#),
            r#"quote \" backslash \\ "#
        );
        assert_eq!(escape("tab\tnewline\n"), "tab\\tnewline\\n");
        assert_eq!(escape("bell\u{7}"), "bell\\u0007");
    }
}
