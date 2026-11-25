//! Source handling, diagnostics, lexing and parsing for Sedge.

pub mod ast;
pub mod diag;
pub mod lexer;
pub mod parser;
pub mod source;
pub mod token;

pub use diag::{Diagnostic, Severity};
pub use source::{FileId, SourceMap, Span};
