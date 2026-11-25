//! Name resolution, type inference and pattern checking for Sedge.

mod check;
pub mod exhaust;
pub mod typed;
pub mod types;

pub use check::{check, suggest, CheckResult};
pub use typed::*;
pub use types::{AdtId, ParamId, Ty, TypeCon};

use sedge_syntax::ast::Module;
use sedge_syntax::parser::parse_module;
use sedge_syntax::{Diagnostic, FileId, SourceMap};

/// The standard prelude, checked before every program.
pub const PRELUDE: &str = include_str!("prelude.sg");

pub const PRELUDE_NAME: &str = "<prelude>";

/// A parsed and checked program.
pub struct Analysis {
    pub map: SourceMap,
    /// The prelude first, then the user's files in order.
    pub modules: Vec<Module>,
    pub files: Vec<FileId>,
    pub typed: Typed,
    pub diagnostics: Vec<Diagnostic>,
}

impl Analysis {
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(|d| d.is_error())
    }

    pub fn render_diagnostics(&self, color: bool) -> String {
        self.diagnostics
            .iter()
            .map(|d| d.render(&self.map, color))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Parses and checks the given files together with the prelude.
pub fn analyze(files: &[(String, String)]) -> Analysis {
    let mut map = SourceMap::new();
    let mut ids = vec![map.add(PRELUDE_NAME, PRELUDE)];
    for (name, text) in files {
        ids.push(map.add(name.clone(), text.clone()));
    }
    let mut next_id = 0;
    let mut modules = Vec::new();
    let mut diagnostics = Vec::new();
    for &id in &ids {
        let f = map.get(id);
        let parsed = parse_module(&f.text, f.base, &mut next_id);
        diagnostics.extend(parsed.diagnostics);
        modules.push(parsed.module);
    }
    let parse_failed = diagnostics.iter().any(|d| d.is_error());
    let typed = if parse_failed {
        Typed::default()
    } else {
        let refs: Vec<&Module> = modules.iter().collect();
        let result = check(&refs);
        diagnostics.extend(result.diagnostics);
        result.typed
    };
    Analysis {
        map,
        modules,
        files: ids,
        typed,
        diagnostics,
    }
}

#[cfg(test)]
mod tests;
