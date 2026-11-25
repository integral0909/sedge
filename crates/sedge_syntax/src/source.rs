//! Source files and positions.
//!
//! Every file added to a [`SourceMap`] gets its own range of global byte
//! offsets, so a [`Span`] is just two numbers and still knows which file it
//! points into.

use std::fmt;

/// A half-open range of global byte offsets.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Span {
    pub lo: u32,
    pub hi: u32,
}

impl Span {
    pub const DUMMY: Span = Span { lo: 0, hi: 0 };

    pub fn new(lo: u32, hi: u32) -> Span {
        Span { lo, hi }
    }

    pub fn to(self, other: Span) -> Span {
        Span {
            lo: self.lo.min(other.lo),
            hi: self.hi.max(other.hi),
        }
    }

    pub fn contains(self, offset: u32) -> bool {
        self.lo <= offset && offset <= self.hi
    }

    pub fn len(self) -> u32 {
        self.hi - self.lo
    }

    pub fn is_empty(self) -> bool {
        self.lo == self.hi
    }
}

impl fmt::Debug for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.lo, self.hi)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FileId(pub u32);

pub struct SourceFile {
    pub id: FileId,
    pub name: String,
    pub text: String,
    /// Global offset of the first byte.
    pub base: u32,
    /// Local byte offsets of the start of each line.
    line_starts: Vec<u32>,
}

/// A resolved position: 1-based line, 1-based column counted in characters.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LineCol {
    pub line: u32,
    pub col: u32,
}

impl SourceFile {
    fn new(id: FileId, name: String, text: String, base: u32) -> SourceFile {
        let mut line_starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i as u32 + 1);
            }
        }
        SourceFile {
            id,
            name,
            text,
            base,
            line_starts,
        }
    }

    pub fn end(&self) -> u32 {
        self.base + self.text.len() as u32
    }

    pub fn span(&self) -> Span {
        Span::new(self.base, self.end())
    }

    /// 0-based line index containing a local offset.
    pub fn line_index(&self, local: u32) -> usize {
        match self.line_starts.binary_search(&local) {
            Ok(i) => i,
            Err(i) => i - 1,
        }
    }

    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// Text of a 0-based line, without its line ending.
    pub fn line_text(&self, line: usize) -> &str {
        let start = self.line_starts[line] as usize;
        let end = self
            .line_starts
            .get(line + 1)
            .map(|&e| e as usize)
            .unwrap_or(self.text.len());
        self.text[start..end].trim_end_matches(['\n', '\r'])
    }

    pub fn line_start(&self, line: usize) -> u32 {
        self.line_starts[line]
    }

    pub fn line_col(&self, global: u32) -> LineCol {
        let local = global.saturating_sub(self.base).min(self.text.len() as u32);
        let line = self.line_index(local);
        let start = self.line_starts[line] as usize;
        let col = self.text[start..local as usize].chars().count() as u32 + 1;
        LineCol {
            line: line as u32 + 1,
            col,
        }
    }

    pub fn slice(&self, span: Span) -> &str {
        let lo = (span.lo - self.base) as usize;
        let hi = (span.hi - self.base) as usize;
        &self.text[lo..hi]
    }
}

#[derive(Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

impl SourceMap {
    pub fn new() -> SourceMap {
        SourceMap::default()
    }

    pub fn add(&mut self, name: impl Into<String>, text: impl Into<String>) -> FileId {
        let base = self.files.last().map(|f| f.end() + 1).unwrap_or(0);
        let id = FileId(self.files.len() as u32);
        self.files
            .push(SourceFile::new(id, name.into(), text.into(), base));
        id
    }

    pub fn get(&self, id: FileId) -> &SourceFile {
        &self.files[id.0 as usize]
    }

    pub fn files(&self) -> &[SourceFile] {
        &self.files
    }

    /// The file containing a global offset.
    pub fn lookup(&self, offset: u32) -> &SourceFile {
        let i = match self.files.binary_search_by_key(&offset, |f| f.base) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        &self.files[i]
    }

    pub fn slice(&self, span: Span) -> &str {
        self.lookup(span.lo).slice(span)
    }

    /// "file:line:col" for the start of a span.
    pub fn describe(&self, span: Span) -> String {
        let file = self.lookup(span.lo);
        let lc = file.line_col(span.lo);
        format!("{}:{}:{}", file.name, lc.line, lc.col)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_and_columns() {
        let mut map = SourceMap::new();
        let a = map.add("a.sg", "one\ntwo\n");
        let b = map.add("b.sg", "héllo wörld");
        let fa = map.get(a);
        assert_eq!(fa.line_col(0), LineCol { line: 1, col: 1 });
        assert_eq!(fa.line_col(5), LineCol { line: 2, col: 2 });
        assert_eq!(fa.line_text(1), "two");
        let fb = map.get(b);
        // "héllo " is 7 bytes and 6 characters.
        assert_eq!(fb.line_col(fb.base + 7), LineCol { line: 1, col: 7 });
        assert_eq!(map.lookup(fb.base + 3).name, "b.sg");
        assert_eq!(map.lookup(2).name, "a.sg");
    }
}
