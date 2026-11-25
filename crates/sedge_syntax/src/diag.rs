//! Diagnostics and their terminal rendering.

use crate::source::{SourceMap, Span};
use std::fmt::Write;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Clone, Debug)]
pub struct Label {
    pub span: Span,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    /// The primary location.
    pub span: Span,
    /// Text shown under the primary caret.
    pub label: Option<String>,
    pub secondary: Vec<Label>,
    pub notes: Vec<String>,
}

impl Diagnostic {
    pub fn error(span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            severity: Severity::Error,
            message: message.into(),
            span,
            label: None,
            secondary: Vec::new(),
            notes: Vec::new(),
        }
    }

    pub fn warning(span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            severity: Severity::Warning,
            ..Diagnostic::error(span, message)
        }
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Diagnostic {
        self.label = Some(label.into());
        self
    }

    pub fn with_secondary(mut self, span: Span, message: impl Into<String>) -> Diagnostic {
        self.secondary.push(Label {
            span,
            message: message.into(),
        });
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Diagnostic {
        self.notes.push(note.into());
        self
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }

    /// Renders the diagnostic with source excerpts, in the style of rustc.
    pub fn render(&self, map: &SourceMap, color: bool) -> String {
        let (red, yellow, blue, bold, reset) = if color {
            ("\x1b[31m", "\x1b[33m", "\x1b[34m", "\x1b[1m", "\x1b[0m")
        } else {
            ("", "", "", "", "")
        };
        let (kind, kcolor) = match self.severity {
            Severity::Error => ("error", red),
            Severity::Warning => ("warning", yellow),
        };
        let mut out = String::new();
        let _ = writeln!(out, "{bold}{kcolor}{kind}{reset}{bold}: {}{reset}", self.message);
        let mut labels = vec![(self.span, self.label.clone().unwrap_or_default(), true)];
        for l in &self.secondary {
            labels.push((l.span, l.message.clone(), false));
        }
        let gutter = labels
            .iter()
            .map(|(s, _, _)| {
                let f = map.lookup(s.lo);
                f.line_col(s.lo).line.to_string().len()
            })
            .max()
            .unwrap_or(1);
        let pad = " ".repeat(gutter);
        let _ = writeln!(out, "{pad}{blue}-->{reset} {}", map.describe(self.span));
        for (span, message, primary) in labels {
            let file = map.lookup(span.lo);
            let start = file.line_col(span.lo);
            let end = file.line_col(span.hi.max(span.lo));
            let line = file.line_text(start.line as usize - 1);
            let width = if end.line == start.line {
                (end.col.saturating_sub(start.col)).max(1) as usize
            } else {
                (line.chars().count() + 1).saturating_sub(start.col as usize).max(1)
            };
            let marker = if primary { "^" } else { "-" };
            let mcolor = if primary { kcolor } else { blue };
            let _ = writeln!(out, "{pad} {blue}|{reset}");
            let _ = writeln!(
                out,
                "{blue}{:>gutter$} |{reset} {}",
                start.line,
                line.replace('\t', "    ")
            );
            let indent: String = line
                .chars()
                .take(start.col as usize - 1)
                .map(|c| if c == '\t' { "    " } else { " " })
                .collect();
            let _ = writeln!(
                out,
                "{pad} {blue}|{reset} {indent}{mcolor}{}{}{reset}",
                marker.repeat(width),
                if message.is_empty() {
                    String::new()
                } else {
                    format!(" {message}")
                }
            );
        }
        for note in &self.notes {
            let _ = writeln!(out, "{pad} {blue}={reset} {bold}note{reset}: {note}");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_excerpt() {
        let mut map = SourceMap::new();
        let id = map.add("main.sg", "fn main() {\n    let x = 1 + true\n}\n");
        let base = map.get(id).base;
        let d = Diagnostic::error(Span::new(base + 28, base + 32), "mismatched types")
            .with_label("expected Int, found Bool")
            .with_note("both operands of `+` must have the same type");
        let text = d.render(&map, false);
        assert_eq!(
            text,
            "error: mismatched types\n --> main.sg:2:17\n  |\n2 |     let x = 1 + true\n  |                 ^^^^ expected Int, found Bool\n  = note: both operands of `+` must have the same type\n"
        );
    }
}
