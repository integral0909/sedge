//! A language server for Sedge over stdio.
//!
//! Every change re-analyzes the whole document (checking is fast enough
//! that incremental work is not needed yet). Hover, go-to-definition and
//! references come from the checker's table of name uses. Completion uses
//! the last version of the document that checked cleanly, because the text
//! being typed usually does not parse.

pub mod json;

use json::Json;
use sedge_check::{analyze, Analysis, Def, Ty, TypeCon};
use sedge_syntax::ast::Item;
use sedge_syntax::{Severity, Span};
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::rc::Rc;

pub fn serve(input: &mut impl BufRead, output: &mut impl Write) -> io::Result<()> {
    let mut server = Server::default();
    while let Some(msg) = read_message(input)? {
        let Ok(req) = Json::parse(&msg) else {
            continue;
        };
        let replies = server.handle(&req);
        for r in replies {
            write_message(output, &r.to_string())?;
        }
        if server.exit {
            break;
        }
    }
    Ok(())
}

pub fn read_message(input: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            if length.is_some() {
                break;
            }
            continue;
        }
        if let Some(v) = line.strip_prefix("Content-Length:") {
            length = v.trim().parse::<usize>().ok();
        }
    }
    let mut buf = vec![0; length.unwrap()];
    input.read_exact(&mut buf)?;
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

pub fn write_message(output: &mut impl Write, body: &str) -> io::Result<()> {
    write!(output, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
    output.flush()
}

struct Doc {
    text: String,
    analysis: Rc<Analysis>,
    /// The last analysis without errors, for completion.
    good: Option<Rc<Analysis>>,
}

#[derive(Default)]
pub struct Server {
    docs: HashMap<String, Doc>,
    shutdown: bool,
    pub exit: bool,
}

fn response(id: &Json, result: Json) -> Json {
    Json::obj(vec![
        ("jsonrpc", Json::str("2.0")),
        ("id", id.clone()),
        ("result", result),
    ])
}

fn notification(method: &str, params: Json) -> Json {
    Json::obj(vec![
        ("jsonrpc", Json::str("2.0")),
        ("method", Json::str(method)),
        ("params", params),
    ])
}

fn file_name(uri: &str) -> String {
    uri.rsplit('/').next().unwrap_or(uri).to_string()
}

/// Converts between byte offsets and LSP positions (UTF-16 columns).
struct Lines<'a> {
    text: &'a str,
    starts: Vec<usize>,
}

impl<'a> Lines<'a> {
    fn new(text: &'a str) -> Lines<'a> {
        let mut starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i + 1);
            }
        }
        Lines { text, starts }
    }

    fn position(&self, offset: usize) -> Json {
        let offset = offset.min(self.text.len());
        let line = match self.starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let col: usize = self.text[self.starts[line]..offset]
            .chars()
            .map(|c| c.len_utf16())
            .sum();
        Json::obj(vec![
            ("line", Json::num(line as u32)),
            ("character", Json::num(col as u32)),
        ])
    }

    fn offset(&self, pos: &Json) -> usize {
        let line = pos.get("line").as_u32().unwrap_or(0) as usize;
        let col = pos.get("character").as_u32().unwrap_or(0) as usize;
        let Some(&start) = self.starts.get(line) else {
            return self.text.len();
        };
        let end = self
            .starts
            .get(line + 1)
            .copied()
            .unwrap_or(self.text.len());
        let mut units = 0;
        for (i, c) in self.text[start..end].char_indices() {
            if units >= col {
                return start + i;
            }
            units += c.len_utf16();
        }
        end.min(self.text.len())
    }

    fn range(&self, lo: usize, hi: usize) -> Json {
        Json::obj(vec![
            ("start", self.position(lo)),
            ("end", self.position(hi)),
        ])
    }
}

impl Doc {
    fn base(&self) -> u32 {
        self.analysis.map.get(self.analysis.files[1]).base
    }

    /// Local byte range of a global span, if it is in this document.
    fn local(&self, a: &Analysis, span: Span) -> Option<(usize, usize)> {
        let f = a.map.get(a.files[1]);
        if span.lo < f.base || span.hi > f.end() {
            return None;
        }
        Some(((span.lo - f.base) as usize, (span.hi - f.base) as usize))
    }
}

impl Server {
    pub fn handle(&mut self, req: &Json) -> Vec<Json> {
        let method = req.get("method").as_str().unwrap_or("");
        let id = req.get("id");
        let params = req.get("params");
        match method {
            "initialize" => vec![response(
                id,
                Json::obj(vec![
                    (
                        "capabilities",
                        Json::obj(vec![
                            ("textDocumentSync", Json::num(1)),
                            ("hoverProvider", Json::Bool(true)),
                            ("definitionProvider", Json::Bool(true)),
                            ("referencesProvider", Json::Bool(true)),
                            ("documentSymbolProvider", Json::Bool(true)),
                            (
                                "completionProvider",
                                Json::obj(vec![(
                                    "triggerCharacters",
                                    Json::Arr(vec![Json::str(".")]),
                                )]),
                            ),
                        ]),
                    ),
                    (
                        "serverInfo",
                        Json::obj(vec![
                            ("name", Json::str("sedge")),
                            ("version", Json::str(env!("CARGO_PKG_VERSION"))),
                        ]),
                    ),
                ]),
            )],
            "shutdown" => {
                self.shutdown = true;
                vec![response(id, Json::Null)]
            }
            "exit" => {
                self.exit = true;
                vec![]
            }
            "textDocument/didOpen" => {
                let uri = params
                    .at(&["textDocument", "uri"])
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                let text = params
                    .at(&["textDocument", "text"])
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                self.update(uri, text)
            }
            "textDocument/didChange" => {
                let uri = params
                    .at(&["textDocument", "uri"])
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                let text = match params.get("contentChanges") {
                    Json::Arr(changes) => changes
                        .last()
                        .and_then(|c| c.get("text").as_str())
                        .unwrap_or("")
                        .to_string(),
                    _ => return vec![],
                };
                self.update(uri, text)
            }
            "textDocument/didClose" => {
                let uri = params
                    .at(&["textDocument", "uri"])
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                self.docs.remove(&uri);
                vec![notification(
                    "textDocument/publishDiagnostics",
                    Json::obj(vec![
                        ("uri", Json::str(uri)),
                        ("diagnostics", Json::Arr(vec![])),
                    ]),
                )]
            }
            "textDocument/hover" => vec![response(id, self.hover(params).unwrap_or(Json::Null))],
            "textDocument/definition" => {
                vec![response(id, self.definition(params).unwrap_or(Json::Null))]
            }
            "textDocument/references" => vec![response(
                id,
                self.references(params).unwrap_or(Json::Arr(vec![])),
            )],
            "textDocument/documentSymbol" => vec![response(
                id,
                self.symbols(params).unwrap_or(Json::Arr(vec![])),
            )],
            "textDocument/completion" => vec![response(
                id,
                self.completion(params).unwrap_or(Json::Arr(vec![])),
            )],
            _ => {
                if !id.is_null() {
                    // Unknown requests get "method not found".
                    vec![Json::obj(vec![
                        ("jsonrpc", Json::str("2.0")),
                        ("id", id.clone()),
                        (
                            "error",
                            Json::obj(vec![
                                ("code", Json::num(-32601)),
                                ("message", Json::str(format!("unknown method {method}"))),
                            ]),
                        ),
                    ])]
                } else {
                    vec![]
                }
            }
        }
    }

    fn update(&mut self, uri: String, text: String) -> Vec<Json> {
        let analysis = Rc::new(analyze(&[(file_name(&uri), text.clone())]));
        let good = if analysis.has_errors() {
            self.docs.remove(&uri).and_then(|d| d.good)
        } else {
            Some(analysis.clone())
        };
        let doc = Doc {
            text,
            analysis,
            good,
        };
        let lines = Lines::new(&doc.text);
        let mut diags = Vec::new();
        for d in &doc.analysis.diagnostics {
            let Some((lo, hi)) = doc.local(&doc.analysis, d.span) else {
                continue;
            };
            let mut message = d.message.clone();
            if let Some(l) = &d.label {
                if !l.is_empty() {
                    message.push_str(": ");
                    message.push_str(l);
                }
            }
            for n in &d.notes {
                message.push_str("\nnote: ");
                message.push_str(n);
            }
            diags.push(Json::obj(vec![
                ("range", lines.range(lo, hi.max(lo))),
                (
                    "severity",
                    Json::num(match d.severity {
                        Severity::Error => 1,
                        Severity::Warning => 2,
                    }),
                ),
                ("source", Json::str("sedge")),
                ("message", Json::str(message)),
            ]));
        }
        let out = notification(
            "textDocument/publishDiagnostics",
            Json::obj(vec![
                ("uri", Json::str(uri.clone())),
                ("diagnostics", Json::Arr(diags)),
            ]),
        );
        self.docs.insert(uri, doc);
        vec![out]
    }

    fn doc_and_offset(&self, params: &Json) -> Option<(&Doc, u32)> {
        let uri = params.at(&["textDocument", "uri"]).as_str()?;
        let doc = self.docs.get(uri)?;
        let lines = Lines::new(&doc.text);
        let off = lines.offset(params.get("position"));
        Some((doc, doc.base() + off as u32))
    }

    /// The innermost name use under the cursor.
    fn use_at(doc: &Doc, offset: u32) -> Option<&sedge_check::Use> {
        doc.analysis
            .typed
            .uses
            .iter()
            .filter(|u| u.span.lo <= offset && offset <= u.span.hi && !u.span.is_empty())
            .min_by_key(|u| u.span.len())
    }

    fn def_span(a: &Analysis, def: Def) -> Span {
        let t = &a.typed;
        match def {
            Def::Local(l) => t.locals[l as usize].span,
            Def::Fn(f) => t.fns[f as usize].name_span,
            Def::Adt(id) => t.adts[id as usize].span,
            Def::Variant(id, v) => t.adts[id as usize].variants()[v as usize].span,
            Def::Field(id, f) => t.adts[id as usize].fields()[f as usize].span,
            Def::Param(p) => t.params[p as usize].span,
        }
    }

    fn describe(a: &Analysis, def: Def, ty: &Ty) -> (String, Option<String>) {
        let t = &a.typed;
        match def {
            Def::Local(l) => {
                let d = &t.locals[l as usize];
                let kw = if d.mutable { "var" } else { "let" };
                (format!("{kw} {}: {}", d.name, t.show(&d.ty)), None)
            }
            Def::Fn(f) => (t.signature(f), t.fns[f as usize].doc.clone()),
            Def::Adt(id) => {
                let adt = &t.adts[id as usize];
                let kw = if adt.is_enum() { "enum" } else { "struct" };
                let params: Vec<&str> = adt
                    .params
                    .iter()
                    .map(|p| t.params[*p as usize].name.as_str())
                    .collect();
                let gen = if params.is_empty() {
                    String::new()
                } else {
                    format!("[{}]", params.join(", "))
                };
                let body = if adt.is_enum() {
                    adt.variants()
                        .iter()
                        .map(|v| {
                            if v.fields.is_empty() {
                                format!("    {},", v.name)
                            } else {
                                let fs: Vec<String> = v.fields.iter().map(|f| t.show(f)).collect();
                                format!("    {}({}),", v.name, fs.join(", "))
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                } else {
                    adt.fields()
                        .iter()
                        .map(|f| format!("    {}: {},", f.name, t.show(&f.ty)))
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                (
                    format!("{kw} {}{gen} {{\n{body}\n}}", adt.name),
                    adt.doc.clone(),
                )
            }
            Def::Variant(id, v) => {
                let adt = &t.adts[id as usize];
                let var = &adt.variants()[v as usize];
                let fields: Vec<String> = var.fields.iter().map(|f| t.show(f)).collect();
                let sig = if fields.is_empty() {
                    format!("{}.{}", adt.name, var.name)
                } else {
                    format!("{}.{}({})", adt.name, var.name, fields.join(", "))
                };
                (sig, var.doc.clone())
            }
            Def::Field(id, f) => {
                let field = &t.adts[id as usize].fields()[f as usize];
                (
                    format!(
                        "{}.{}: {}",
                        t.adts[id as usize].name,
                        field.name,
                        t.show(ty)
                    ),
                    field.doc.clone(),
                )
            }
            Def::Param(p) => (
                format!("type parameter {}", t.params[p as usize].name),
                None,
            ),
        }
    }

    fn hover(&self, params: &Json) -> Option<Json> {
        let (doc, offset) = self.doc_and_offset(params)?;
        let u = Self::use_at(doc, offset)?;
        let (code, docs) = Self::describe(&doc.analysis, u.def, &u.ty);
        let mut value = format!("```sedge\n{code}\n```");
        if let Some(d) = docs {
            value.push_str("\n\n");
            value.push_str(&d);
        }
        let (lo, hi) = doc.local(&doc.analysis, u.span)?;
        let lines = Lines::new(&doc.text);
        Some(Json::obj(vec![
            (
                "contents",
                Json::obj(vec![
                    ("kind", Json::str("markdown")),
                    ("value", Json::str(value)),
                ]),
            ),
            ("range", lines.range(lo, hi)),
        ]))
    }

    fn definition(&self, params: &Json) -> Option<Json> {
        let (doc, offset) = self.doc_and_offset(params)?;
        let u = Self::use_at(doc, offset)?;
        let span = Self::def_span(&doc.analysis, u.def);
        let (lo, hi) = doc.local(&doc.analysis, span)?; // prelude definitions are not files
        let uri = params.at(&["textDocument", "uri"]).clone();
        let lines = Lines::new(&doc.text);
        Some(Json::obj(vec![
            ("uri", uri),
            ("range", lines.range(lo, hi)),
        ]))
    }

    fn references(&self, params: &Json) -> Option<Json> {
        let (doc, offset) = self.doc_and_offset(params)?;
        let def = Self::use_at(doc, offset)?.def;
        let uri = params.at(&["textDocument", "uri"]).clone();
        let lines = Lines::new(&doc.text);
        let mut seen = Vec::new();
        let mut out = Vec::new();
        for u in &doc.analysis.typed.uses {
            if u.def != def || seen.contains(&u.span) {
                continue;
            }
            seen.push(u.span);
            if let Some((lo, hi)) = doc.local(&doc.analysis, u.span) {
                out.push(Json::obj(vec![
                    ("uri", uri.clone()),
                    ("range", lines.range(lo, hi)),
                ]));
            }
        }
        Some(Json::Arr(out))
    }

    fn symbols(&self, params: &Json) -> Option<Json> {
        let uri = params.at(&["textDocument", "uri"]).as_str()?;
        let doc = self.docs.get(uri)?;
        let a = &doc.analysis;
        let lines = Lines::new(&doc.text);
        let module = a.modules.get(1)?;
        let sym =
            |name: &str, kind: u32, span: Span, sel: Span, children: Vec<Json>| -> Option<Json> {
                let (lo, hi) = doc.local(a, span)?;
                let (slo, shi) = doc.local(a, sel)?;
                Some(Json::obj(vec![
                    ("name", Json::str(name)),
                    ("kind", Json::num(kind)),
                    ("range", lines.range(lo, hi)),
                    ("selectionRange", lines.range(slo, shi)),
                    ("children", Json::Arr(children)),
                ]))
            };
        let mut out = Vec::new();
        for item in &module.items {
            let s = match item {
                Item::Fn(f) => sym(&f.name.name, 12, f.span, f.name.span, vec![]),
                Item::Struct(s) => {
                    let kids = s
                        .fields
                        .iter()
                        .filter_map(|f| {
                            sym(
                                &f.name.name,
                                8,
                                f.name.span.to(f.ty.span),
                                f.name.span,
                                vec![],
                            )
                        })
                        .collect();
                    sym(&s.name.name, 23, s.span, s.name.span, kids)
                }
                Item::Enum(e) => {
                    let kids = e
                        .variants
                        .iter()
                        .filter_map(|v| sym(&v.name.name, 22, v.name.span, v.name.span, vec![]))
                        .collect();
                    sym(&e.name.name, 10, e.span, e.name.span, kids)
                }
                Item::Impl(i) => {
                    let kids = i
                        .methods
                        .iter()
                        .filter_map(|m| sym(&m.name.name, 6, m.span, m.name.span, vec![]))
                        .collect();
                    sym(
                        &format!("impl {}", i.target.name),
                        5,
                        i.span,
                        i.target.span,
                        kids,
                    )
                }
                Item::Test(t) => sym(&format!("test \"{}\"", t.name), 12, t.span, t.span, vec![]),
            };
            out.extend(s);
        }
        Some(Json::Arr(out))
    }

    fn completion(&self, params: &Json) -> Option<Json> {
        let uri = params.at(&["textDocument", "uri"]).as_str()?;
        let doc = self.docs.get(uri)?;
        let lines = Lines::new(&doc.text);
        let cursor = lines.offset(params.get("position"));
        let before = &doc.text[..cursor];
        // Strip the partial word being typed.
        let word_start = before
            .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
            .map(|i| i + 1)
            .unwrap_or(0);
        let head = &before[..word_start];
        let a = doc.good.as_ref().unwrap_or(&doc.analysis);
        let t = &a.typed;
        let item = |label: &str, kind: u32, detail: String| {
            Json::obj(vec![
                ("label", Json::str(label)),
                ("kind", Json::num(kind)),
                ("detail", Json::str(detail)),
            ])
        };
        let mut out = Vec::new();
        if let Some(recv_text) = head.strip_suffix('.') {
            let recv: String = recv_text
                .chars()
                .rev()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            if recv.is_empty() {
                return Some(Json::Arr(out));
            }
            // A type name: its variants and static functions.
            if let Some(adt) = t.adts.iter().position(|d| d.name == recv) {
                for v in t.adts[adt].variants() {
                    out.push(item(&v.name, 20, format!("{}.{}", recv, v.name)));
                }
                for ((con, name), f) in &t.methods {
                    if *con == TypeCon::Adt(adt as u32) && !t.fns[*f as usize].has_self {
                        out.push(item(name, 3, t.signature(*f)));
                    }
                }
                return Some(Json::Arr(out));
            }
            // A value: find the closest earlier use of that name to get its type.
            let user_base = a.map.get(a.files[1]).base;
            let at = user_base + word_start as u32;
            let ty = t
                .uses
                .iter()
                .filter(|u| u.span.lo < at && a.map.slice(u.span) == recv)
                .max_by_key(|u| u.span.lo)
                .map(|u| u.ty.clone())?;
            if let Ty::Adt(id, _) = &ty {
                for f in t.adts[*id as usize].fields() {
                    out.push(item(&f.name, 5, t.show(&f.ty)));
                }
            }
            if let Some(con) = ty.con() {
                let mut methods: Vec<(&String, &u32)> = t
                    .methods
                    .iter()
                    .filter(|((c, _), f)| *c == con && t.fns[**f as usize].has_self)
                    .map(|((_, n), f)| (n, f))
                    .collect();
                methods.sort();
                for (name, f) in methods {
                    out.push(item(name, 2, t.signature(*f)));
                }
            }
            return Some(Json::Arr(out));
        }
        for kw in [
            "fn", "let", "var", "if", "else", "while", "for", "in", "return", "break", "continue",
            "match", "struct", "enum", "impl", "true", "false", "self",
        ] {
            out.push(item(kw, 14, "keyword".into()));
        }
        let mut seen = std::collections::HashSet::new();
        let user = a.map.get(a.files[1]);
        let at = user.base + cursor as u32;
        for l in t.locals.iter().rev() {
            if l.span.lo >= user.base && l.span.lo < at && seen.insert(l.name.clone()) {
                out.push(item(&l.name, 6, t.show(&l.ty)));
            }
        }
        for (name, f) in &t.globals {
            if seen.insert(name.clone()) {
                out.push(item(name, 3, t.signature(*f)));
            }
        }
        for adt in &t.adts {
            if seen.insert(adt.name.clone()) {
                out.push(item(
                    &adt.name,
                    if adt.is_enum() { 13 } else { 22 },
                    String::new(),
                ));
            }
            for v in adt.variants() {
                if seen.insert(v.name.clone()) {
                    out.push(item(&v.name, 20, format!("{}.{}", adt.name, v.name)));
                }
            }
        }
        Some(Json::Arr(out))
    }
}

#[cfg(test)]
mod tests;
