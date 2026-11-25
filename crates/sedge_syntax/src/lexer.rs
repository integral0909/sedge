//! The lexer, including automatic semicolon insertion.
//!
//! A line break ends a statement when the last token on the line could end
//! one (an identifier, a literal, a closing bracket, `return` and so on) and
//! the break is not inside parentheses or square brackets. A line that
//! starts with `.` continues the previous one, so method chains can be split
//! across lines.

use crate::diag::Diagnostic;
use crate::source::Span;
use crate::token::{StrPart, Token, TokenKind};

pub struct Lexed {
    pub tokens: Vec<Token>,
    /// Doc comments (`///`), as (span of the comment block, text).
    pub docs: Vec<(Span, String)>,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn lex(text: &str, base: u32) -> Lexed {
    let mut lx = Lexer {
        src: text.as_bytes(),
        text,
        pos: 0,
        base,
        tokens: Vec::new(),
        docs: Vec::new(),
        diagnostics: Vec::new(),
        delims: Vec::new(),
        pending_newline: false,
    };
    lx.run();
    Lexed {
        tokens: lx.tokens,
        docs: lx.docs,
        diagnostics: lx.diagnostics,
    }
}

struct Lexer<'a> {
    src: &'a [u8],
    text: &'a str,
    pos: usize,
    base: u32,
    tokens: Vec<Token>,
    docs: Vec<(Span, String)>,
    diagnostics: Vec<Diagnostic>,
    delims: Vec<u8>,
    pending_newline: bool,
}

impl<'a> Lexer<'a> {
    fn span(&self, start: usize, end: usize) -> Span {
        Span::new(self.base + start as u32, self.base + end as u32)
    }

    fn peek(&self) -> u8 {
        self.src.get(self.pos).copied().unwrap_or(0)
    }

    fn peek_at(&self, n: usize) -> u8 {
        self.src.get(self.pos + n).copied().unwrap_or(0)
    }

    fn error(&mut self, start: usize, end: usize, msg: impl Into<String>) {
        let span = self.span(start, end);
        self.diagnostics.push(Diagnostic::error(span, msg));
    }

    fn asi_allowed(&self) -> bool {
        !matches!(self.delims.last(), Some(b'(') | Some(b'['))
    }

    /// Inserts a semicolon for a pending line break, if the rules allow it.
    fn flush_newline(&mut self, next: &TokenKind) {
        if !self.pending_newline {
            return;
        }
        self.pending_newline = false;
        if matches!(next, TokenKind::Dot) || !self.asi_allowed() {
            return;
        }
        if let Some(last) = self.tokens.last() {
            if last.kind.ends_statement() {
                let at = last.span.hi;
                self.tokens.push(Token {
                    kind: TokenKind::Semi,
                    span: Span::new(at, at),
                    implicit: true,
                });
            }
        }
    }

    fn push(&mut self, kind: TokenKind, start: usize, end: usize) {
        self.flush_newline(&kind);
        self.tokens.push(Token {
            kind,
            span: self.span(start, end),
            implicit: false,
        });
    }

    fn run(&mut self) {
        loop {
            self.skip_trivia();
            let start = self.pos;
            if self.pos >= self.src.len() {
                self.pending_newline = true;
                self.delims.clear();
                self.flush_newline(&TokenKind::Eof);
                let end = self.span(self.src.len(), self.src.len());
                self.tokens.push(Token {
                    kind: TokenKind::Eof,
                    span: end,
                    implicit: false,
                });
                return;
            }
            let c = self.peek();
            if c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 {
                self.ident(start);
            } else if c.is_ascii_digit() {
                self.number(start);
            } else if c == b'"' {
                self.string(start);
            } else {
                self.punct(start);
            }
        }
    }

    fn skip_trivia(&mut self) {
        loop {
            let c = self.peek();
            match c {
                b'\n' => {
                    self.pending_newline = true;
                    self.pos += 1;
                }
                b' ' | b'\t' | b'\r' => self.pos += 1,
                b'/' if self.peek_at(1) == b'/' => {
                    let start = self.pos;
                    while self.pos < self.src.len() && self.peek() != b'\n' {
                        self.pos += 1;
                    }
                    let line = &self.text[start..self.pos];
                    if let Some(doc) = line.strip_prefix("///") {
                        if !doc.starts_with('/') {
                            self.add_doc(start, doc);
                        }
                    }
                }
                b'/' if self.peek_at(1) == b'*' => {
                    let start = self.pos;
                    self.pos += 2;
                    let mut depth = 1;
                    while depth > 0 {
                        if self.pos >= self.src.len() {
                            self.error(start, start + 2, "unterminated block comment");
                            return;
                        }
                        if self.peek() == b'/' && self.peek_at(1) == b'*' {
                            depth += 1;
                            self.pos += 2;
                        } else if self.peek() == b'*' && self.peek_at(1) == b'/' {
                            depth -= 1;
                            self.pos += 2;
                        } else {
                            if self.peek() == b'\n' {
                                self.pending_newline = true;
                            }
                            self.pos += 1;
                        }
                    }
                }
                _ => return,
            }
        }
    }

    fn add_doc(&mut self, start: usize, line: &str) {
        let text = line.strip_prefix(' ').unwrap_or(line).to_string();
        let span = self.span(start, self.pos);
        if let Some((prev, prev_text)) = self.docs.last_mut() {
            // Merge consecutive doc lines (only whitespace in between).
            let gap_lo = (prev.hi - self.base) as usize;
            if self.text[gap_lo..start].trim().is_empty() {
                prev.hi = span.hi;
                prev_text.push('\n');
                prev_text.push_str(&text);
                return;
            }
        }
        self.docs.push((span, text));
    }

    fn ident(&mut self, start: usize) {
        while self.pos < self.src.len() {
            let c = self.peek();
            if c.is_ascii_alphanumeric() || c == b'_' {
                self.pos += 1;
            } else if c >= 0x80 {
                // Accept any non-ASCII letter as part of an identifier.
                let ch = self.text[self.pos..].chars().next().unwrap();
                if ch.is_alphanumeric() {
                    self.pos += ch.len_utf8();
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        if self.pos == start {
            // A non-ASCII character that is not a letter.
            let ch = self.text[self.pos..].chars().next().unwrap();
            self.pos += ch.len_utf8();
            self.error(start, self.pos, format!("unexpected character `{ch}`"));
            return;
        }
        let word = &self.text[start..self.pos];
        let kind = TokenKind::keyword(word).unwrap_or_else(|| TokenKind::Ident(word.to_string()));
        self.push(kind, start, self.pos);
    }

    fn number(&mut self, start: usize) {
        let after_dot = matches!(self.tokens.last(), Some(t) if t.kind == TokenKind::Dot)
            && !self.pending_newline;
        if self.peek() == b'0' && matches!(self.peek_at(1), b'x' | b'b' | b'o') {
            let radix = match self.peek_at(1) {
                b'x' => 16,
                b'b' => 2,
                _ => 8,
            };
            self.pos += 2;
            let digits_start = self.pos;
            while self.peek().is_ascii_alphanumeric() || self.peek() == b'_' {
                self.pos += 1;
            }
            let digits: String = self.text[digits_start..self.pos]
                .chars()
                .filter(|&c| c != '_')
                .collect();
            match i64::from_str_radix(&digits, radix) {
                Ok(v) => self.push(TokenKind::Int(v), start, self.pos),
                Err(_) => {
                    self.error(start, self.pos, "invalid integer literal");
                    self.push(TokenKind::Int(0), start, self.pos);
                }
            }
            return;
        }
        while self.peek().is_ascii_digit() || self.peek() == b'_' {
            self.pos += 1;
        }
        let mut is_float = false;
        if !after_dot && self.peek() == b'.' && self.peek_at(1).is_ascii_digit() {
            is_float = true;
            self.pos += 1;
            while self.peek().is_ascii_digit() || self.peek() == b'_' {
                self.pos += 1;
            }
        }
        if !after_dot && matches!(self.peek(), b'e' | b'E') {
            let save = self.pos;
            self.pos += 1;
            if matches!(self.peek(), b'+' | b'-') {
                self.pos += 1;
            }
            if self.peek().is_ascii_digit() {
                is_float = true;
                while self.peek().is_ascii_digit() {
                    self.pos += 1;
                }
            } else {
                self.pos = save;
            }
        }
        let text: String = self.text[start..self.pos]
            .chars()
            .filter(|&c| c != '_')
            .collect();
        if is_float {
            let v = text.parse::<f64>().unwrap_or(0.0);
            self.push(TokenKind::Float(v), start, self.pos);
        } else {
            match text.parse::<i64>() {
                Ok(v) => self.push(TokenKind::Int(v), start, self.pos),
                Err(_) => {
                    // Allow i64::MIN to be written as a negated literal.
                    if text == "9223372036854775808" {
                        self.push(TokenKind::Int(i64::MIN), start, self.pos);
                    } else {
                        self.error(start, self.pos, "integer literal is too large");
                        self.push(TokenKind::Int(0), start, self.pos);
                    }
                }
            }
        }
    }

    fn string(&mut self, start: usize) {
        self.pos += 1;
        let mut parts = Vec::new();
        let mut lit = String::new();
        loop {
            if self.pos >= self.src.len() {
                self.error(start, start + 1, "unterminated string literal");
                break;
            }
            let c = self.peek();
            match c {
                b'"' => {
                    self.pos += 1;
                    break;
                }
                b'\\' => {
                    let esc_start = self.pos;
                    self.pos += 1;
                    let e = self.peek();
                    self.pos += 1;
                    match e {
                        b'n' => lit.push('\n'),
                        b't' => lit.push('\t'),
                        b'r' => lit.push('\r'),
                        b'0' => lit.push('\0'),
                        b'\\' => lit.push('\\'),
                        b'"' => lit.push('"'),
                        b'$' => lit.push('$'),
                        b'u' if self.peek() == b'{' => {
                            self.pos += 1;
                            let hex_start = self.pos;
                            while self.peek().is_ascii_hexdigit() {
                                self.pos += 1;
                            }
                            let hex = &self.text[hex_start..self.pos];
                            let ok = self.peek() == b'}';
                            if ok {
                                self.pos += 1;
                            }
                            match u32::from_str_radix(hex, 16).ok().and_then(char::from_u32) {
                                Some(ch) if ok => lit.push(ch),
                                _ => self.error(esc_start, self.pos, "invalid unicode escape"),
                            }
                        }
                        _ => {
                            let end = (esc_start + 2).min(self.src.len());
                            self.error(esc_start, end, "unknown escape sequence");
                        }
                    }
                }
                b'$' if self.peek_at(1) == b'{' => {
                    if !lit.is_empty() {
                        parts.push(StrPart::Lit(std::mem::take(&mut lit)));
                    }
                    let hole_start = self.pos + 2;
                    self.pos += 2;
                    let mut depth = 1;
                    let mut in_str = false;
                    while self.pos < self.src.len() {
                        let c = self.peek();
                        if in_str {
                            if c == b'\\' {
                                self.pos += 1;
                            } else if c == b'"' {
                                in_str = false;
                            }
                        } else if c == b'"' {
                            in_str = true;
                        } else if c == b'{' {
                            depth += 1;
                        } else if c == b'}' {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        self.pos += 1;
                    }
                    if self.pos >= self.src.len() {
                        self.error(hole_start - 2, hole_start, "unterminated `${` in string");
                        break;
                    }
                    parts.push(StrPart::Hole(self.span(hole_start, self.pos)));
                    self.pos += 1;
                }
                _ => {
                    let ch = self.text[self.pos..].chars().next().unwrap();
                    lit.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
        if !lit.is_empty() || parts.is_empty() {
            parts.push(StrPart::Lit(lit));
        }
        self.push(TokenKind::Str(parts), start, self.pos);
    }

    fn punct(&mut self, start: usize) {
        use TokenKind::*;
        let c = self.peek();
        let n = self.peek_at(1);
        let (kind, len) = match (c, n) {
            (b'-', b'>') => (Arrow, 2),
            (b'=', b'>') => (FatArrow, 2),
            (b'=', b'=') => (EqEq, 2),
            (b'!', b'=') => (NotEq, 2),
            (b'<', b'=') => (Le, 2),
            (b'>', b'=') => (Ge, 2),
            (b'<', b'<') => (Shl, 2),
            (b'>', b'>') => (Shr, 2),
            (b'&', b'&') => (AndAnd, 2),
            (b'|', b'|') => (OrOr, 2),
            (b'+', b'=') => (PlusEq, 2),
            (b'-', b'=') => (MinusEq, 2),
            (b'*', b'=') => (StarEq, 2),
            (b'/', b'=') => (SlashEq, 2),
            (b'%', b'=') => (PercentEq, 2),
            (b'.', b'.') => {
                if self.peek_at(2) == b'=' {
                    (DotDotEq, 3)
                } else {
                    (DotDot, 2)
                }
            }
            (b'(', _) => (LParen, 1),
            (b')', _) => (RParen, 1),
            (b'[', _) => (LBracket, 1),
            (b']', _) => (RBracket, 1),
            (b'{', _) => (LBrace, 1),
            (b'}', _) => (RBrace, 1),
            (b',', _) => (Comma, 1),
            (b'.', _) => (Dot, 1),
            (b':', _) => (Colon, 1),
            (b';', _) => (Semi, 1),
            (b'?', _) => (Question, 1),
            (b'+', _) => (Plus, 1),
            (b'-', _) => (Minus, 1),
            (b'*', _) => (Star, 1),
            (b'/', _) => (Slash, 1),
            (b'%', _) => (Percent, 1),
            (b'&', _) => (Amp, 1),
            (b'|', _) => (Pipe, 1),
            (b'^', _) => (Caret, 1),
            (b'!', _) => (Bang, 1),
            (b'=', _) => (Eq, 1),
            (b'<', _) => (Lt, 1),
            (b'>', _) => (Gt, 1),
            _ => {
                self.pos += 1;
                self.error(start, self.pos, format!("unexpected character `{}`", c as char));
                return;
            }
        };
        self.pos += len;
        // A line break before a closing bracket is judged in the context the
        // bracket closes, so flush it before popping.
        self.flush_newline(&kind);
        match kind {
            LParen => self.delims.push(b'('),
            LBracket => self.delims.push(b'['),
            LBrace => self.delims.push(b'{'),
            RParen | RBracket | RBrace => {
                self.delims.pop();
            }
            _ => {}
        }
        self.push(kind, start, self.pos);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use TokenKind::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        let l = lex(src, 0);
        assert!(l.diagnostics.is_empty(), "{:?}", l.diagnostics);
        l.tokens.into_iter().map(|t| t.kind).collect()
    }

    fn id(s: &str) -> TokenKind {
        Ident(s.to_string())
    }

    #[test]
    fn inserts_semicolons_at_line_ends() {
        assert_eq!(
            kinds("let x = 1\nx + 2\n"),
            vec![Let, id("x"), Eq, Int(1), Semi, id("x"), Plus, Int(2), Semi, Eof]
        );
    }

    #[test]
    fn no_semicolon_after_operator_or_inside_parens() {
        assert_eq!(
            kinds("f(1,\n2) +\n3"),
            vec![id("f"), LParen, Int(1), Comma, Int(2), RParen, Plus, Int(3), Semi, Eof]
        );
    }

    #[test]
    fn leading_dot_continues_line() {
        assert_eq!(
            kinds("xs\n  .len()\n"),
            vec![id("xs"), Dot, id("len"), LParen, RParen, Semi, Eof]
        );
    }

    #[test]
    fn braces_reenable_semicolons_inside_parens() {
        assert_eq!(
            kinds("f(fn(x) {\n x\n})"),
            vec![
                id("f"),
                LParen,
                Fn,
                LParen,
                id("x"),
                RParen,
                LBrace,
                id("x"),
                Semi,
                RBrace,
                RParen,
                Semi,
                Eof
            ]
        );
    }

    #[test]
    fn numbers() {
        assert_eq!(
            kinds("1_000 0xff 0b101 1.5 2e3 1..3 t.0.1"),
            vec![
                Int(1000),
                Int(255),
                Int(5),
                Float(1.5),
                Float(2000.0),
                Int(1),
                DotDot,
                Int(3),
                id("t"),
                Dot,
                Int(0),
                Dot,
                Int(1),
                Semi,
                Eof
            ]
        );
    }

    #[test]
    fn strings_with_holes() {
        let l = lex(r#""a\n${x + 1}b""#, 10);
        match &l.tokens[0].kind {
            Str(parts) => {
                assert_eq!(parts[0], StrPart::Lit("a\n".into()));
                assert_eq!(parts[1], StrPart::Hole(Span::new(16, 21)));
                assert_eq!(parts[2], StrPart::Lit("b".into()));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn nested_string_in_hole() {
        let l = lex(r#""${f("}")}!""#, 0);
        assert!(l.diagnostics.is_empty());
        match &l.tokens[0].kind {
            Str(parts) => assert_eq!(parts.len(), 2),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn doc_comments_merge() {
        let l = lex("/// one\n/// two\nfn f() {}\n// plain\n", 0);
        assert_eq!(l.docs.len(), 1);
        assert_eq!(l.docs[0].1, "one\ntwo");
    }

    #[test]
    fn reports_bad_characters() {
        let l = lex("let x = @", 0);
        assert_eq!(l.diagnostics.len(), 1);
    }
}
