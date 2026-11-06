pub(crate) fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n')
}

pub(crate) fn strip_comments(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    for c in value.chars() {
        if escaped {
            escaped = false;
            if depth == 0 {
                out.push(c);
            }
            continue;
        }
        match c {
            '\\' if quoted || depth > 0 => {
                escaped = true;
                if depth == 0 {
                    out.push(c);
                }
            }
            '"' if depth == 0 => {
                quoted = !quoted;
                out.push(c);
            }
            '(' if !quoted => {
                if depth == 0 {
                    out.push(' ');
                }
                depth += 1;
            }
            ')' if !quoted && depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

pub(crate) fn split_outside_quotes(value: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    let mut escaped = false;
    for (i, c) in value.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            _ if c == sep && !quoted => {
                parts.push(&value[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&value[start..]);
    parts
}

pub(crate) struct Cursor<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl<'a> Cursor<'a> {
    pub(crate) fn new(s: &'a str) -> Self {
        Self {
            chars: s.chars().peekable(),
        }
    }

    pub(crate) fn peek(&mut self) -> Option<char> {
        self.chars.peek().copied()
    }

    pub(crate) fn bump(&mut self) -> Option<char> {
        self.chars.next()
    }

    pub(crate) fn skip_ws(&mut self) {
        while self.peek().is_some_and(is_ws) {
            self.bump();
        }
    }

    pub(crate) fn take_while(&mut self, keep: impl Fn(char) -> bool) -> String {
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if !keep(c) {
                break;
            }
            out.push(c);
            self.bump();
        }
        out
    }

    pub(crate) fn value(&mut self) -> String {
        if self.peek() == Some('"') {
            self.bump();
            let mut out = String::new();
            while let Some(c) = self.bump() {
                match c {
                    '"' => break,
                    '\\' => {
                        if let Some(next) = self.bump() {
                            out.push(next);
                        }
                    }
                    _ => out.push(c),
                }
            }
            out
        } else {
            self.take_while(|c| !is_ws(c))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_nest_and_respect_quotes() {
        assert_eq!(strip_comments("a (b (c) d) e"), "a   e");
        assert_eq!(strip_comments("a \"(b)\" c"), "a \"(b)\" c");
        assert_eq!(strip_comments("a (b \\) c) d"), "a   d");
        assert_eq!(strip_comments("a (unclosed"), "a  ");
    }

    #[test]
    fn splits_outside_quotes() {
        assert_eq!(
            split_outside_quotes("a;\"b;c\";d", ';'),
            ["a", "\"b;c\"", "d"]
        );
        assert_eq!(split_outside_quotes("", ';'), [""]);
    }
}
