//! Name resolution and type inference.
//!
//! Checking runs in passes over all modules (the prelude first): type names,
//! then struct fields and enum variants, then function signatures, then
//! bodies. Bodies are checked with unification-based inference; generic
//! functions are instantiated with fresh variables at each use. Operator
//! constraints that cannot be decided yet (is this `+` on Int or Float?)
//! are recorded as obligations and solved once all bodies are checked.

use crate::exhaust::{self, Ctor, Pat};
use crate::typed::*;
use crate::types::*;
use sedge_syntax::ast::*;
use sedge_syntax::parser::is_block_like;
use sedge_syntax::{Diagnostic, Span};
use std::collections::{HashMap, HashSet};

pub struct CheckResult {
    pub typed: Typed,
    pub diagnostics: Vec<Diagnostic>,
}

/// Checks the modules together. Module 0 is treated as the prelude.
pub fn check(modules: &[&Module]) -> CheckResult {
    let mut c = Checker {
        modules,
        t: Typed::default(),
        u: Unifier::default(),
        diags: Vec::new(),
        type_names: HashMap::new(),
        variants: HashMap::new(),
        scopes: Vec::new(),
        type_scope: Vec::new(),
        ctxs: Vec::new(),
        local_ctx: HashMap::new(),
        obligations: Vec::new(),
        self_type: None,
        module: 0,
    };
    c.run();
    CheckResult {
        typed: c.t,
        diagnostics: c.diags,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum TypeName {
    Never,
    Int,
    Float,
    Bool,
    Str,
    Range,
    Array,
    Map,
    Adt(AdtId),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Oblig {
    Bound(Bound),
    /// Int or Float.
    Num,
    /// Int, Float or String.
    Addable,
}

struct Obligation {
    ty: Ty,
    kind: Oblig,
    span: Span,
    /// What needed it, such as "`<`".
    what: String,
}

/// A function or lambda body being checked.
struct Ctx {
    ret: Ty,
    /// Index of the first scope that belongs to this body.
    scope_base: usize,
    captures: Vec<LocalId>,
    loop_depth: u32,
}

struct Checker<'a> {
    modules: &'a [&'a Module],
    t: Typed,
    u: Unifier,
    diags: Vec<Diagnostic>,
    type_names: HashMap<String, TypeName>,
    variants: HashMap<String, Vec<(AdtId, u32)>>,
    scopes: Vec<HashMap<String, LocalId>>,
    type_scope: Vec<HashMap<String, ParamId>>,
    ctxs: Vec<Ctx>,
    /// Which body (index into `ctxs`) each live local belongs to.
    local_ctx: HashMap<LocalId, usize>,
    obligations: Vec<Obligation>,
    /// The type `Self` stands for inside an impl.
    self_type: Option<Ty>,
    module: usize,
}

impl<'a> Checker<'a> {
    fn run(&mut self) {
        self.collect_types();
        self.collect_adt_bodies();
        self.collect_fns();
        self.check_bodies();
        self.solve_obligations();
        self.finish();
    }

    fn error(&mut self, span: Span, msg: impl Into<String>) -> Diagnostic {
        Diagnostic::error(span, msg)
    }

    fn emit(&mut self, d: Diagnostic) {
        self.diags.push(d);
    }

    fn err(&mut self, span: Span, msg: impl Into<String>) {
        let d = Diagnostic::error(span, msg);
        self.diags.push(d);
    }

    fn show(&self, ty: &Ty) -> String {
        let r = self.u.resolve(ty);
        self.t.show(&r)
    }

    fn mismatch(&mut self, span: Span, expected: &Ty, found: &Ty) {
        let (e, f) = (self.u.resolve(expected), self.u.resolve(found));
        if e.has_error() || f.has_error() {
            return;
        }
        let es = self.t.show(&e);
        let fs = self.t.show(&f);
        let mut d = Diagnostic::error(span, "mismatched types")
            .with_label(format!("expected `{es}`, found `{fs}`"));
        match (&e, &f) {
            (Ty::Float, Ty::Int) => d = d.with_note("convert with `.to_float()`"),
            (Ty::Int, Ty::Float) => d = d.with_note("convert with `.to_int()`, which truncates"),
            (Ty::Str, _) if f != Ty::Str => {
                d = d.with_note("convert a value to a string with `str(x)` or `\"${x}\"`")
            }
            _ => {}
        }
        self.emit(d);
    }

    /// Unifies and reports a mismatch at `span`.
    fn unify_at(&mut self, span: Span, expected: &Ty, found: &Ty) -> bool {
        match self.u.unify(expected, found) {
            Ok(()) => true,
            Err(UnifyError::Mismatch) => {
                self.mismatch(span, expected, found);
                false
            }
            Err(UnifyError::Infinite) => {
                let e = self.show(expected);
                let f = self.show(found);
                self.err(
                    span,
                    format!("cannot build an infinite type: `{e}` would contain `{f}`"),
                );
                false
            }
        }
    }

    fn record(&mut self, id: NodeId, ty: &Ty) {
        self.t.expr_types.insert(id, ty.clone());
    }

    fn add_use(&mut self, span: Span, def: Def, ty: Ty) {
        self.t.uses.push(Use { span, def, ty });
    }

    fn is_prelude(&self) -> bool {
        self.module == 0 && self.modules.len() > 1
    }

    // ----- pass 1: type names --------------------------------------------

    fn collect_types(&mut self) {
        for (name, tn) in [
            ("Never", TypeName::Never),
            ("Int", TypeName::Int),
            ("Float", TypeName::Float),
            ("Bool", TypeName::Bool),
            ("String", TypeName::Str),
            ("Range", TypeName::Range),
            ("Array", TypeName::Array),
            ("Map", TypeName::Map),
        ] {
            self.type_names.insert(name.to_string(), tn);
        }
        for (mi, m) in self.modules.iter().enumerate() {
            self.module = mi;
            for item in &m.items {
                let (name, span, doc, is_enum) = match item {
                    Item::Struct(s) => (&s.name, s.span, s.doc.clone(), false),
                    Item::Enum(e) => (&e.name, e.span, e.doc.clone(), true),
                    _ => continue,
                };
                if self.type_names.contains_key(&name.name) {
                    self.err(
                        name.span,
                        format!("the type `{}` is defined more than once", name.name),
                    );
                    continue;
                }
                let id = self.t.adts.len() as AdtId;
                self.t.adts.push(AdtDef {
                    name: name.name.clone(),
                    params: Vec::new(),
                    kind: if is_enum {
                        AdtKind::Enum(Vec::new())
                    } else {
                        AdtKind::Struct(Vec::new())
                    },
                    span,
                    doc,
                });
                self.type_names.insert(name.name.clone(), TypeName::Adt(id));
                if mi == 0 && name.name == "Option" {
                    self.t.option_adt = Some(id);
                }
                if mi == 0 && name.name == "Result" {
                    self.t.result_adt = Some(id);
                }
                self.add_use(name.span, Def::Adt(id), Ty::Unit);
            }
        }
    }

    fn new_param(&mut self, name: &Ident, bounds: &[Ident]) -> ParamId {
        let mut bs = Vec::new();
        for b in bounds {
            match Bound::from_name(&b.name) {
                Some(bound) => bs.push(bound),
                None => self.err(
                    b.span,
                    format!(
                        "unknown bound `{}`; the available bounds are `Ord`, `Num` and `Key`",
                        b.name
                    ),
                ),
            }
        }
        let id = self.t.params.len() as ParamId;
        self.t.params.push(ParamDef {
            name: name.name.clone(),
            bounds: bs,
            span: name.span,
        });
        self.add_use(name.span, Def::Param(id), Ty::Param(id));
        id
    }

    fn generic_scope(&mut self, params: &[GenericParam]) -> Vec<ParamId> {
        let mut scope = HashMap::new();
        let mut ids = Vec::new();
        for p in params {
            if scope.contains_key(&p.name.name) {
                self.err(
                    p.name.span,
                    format!("duplicate type parameter `{}`", p.name.name),
                );
            }
            let id = self.new_param(&p.name, &p.bounds);
            scope.insert(p.name.name.clone(), id);
            ids.push(id);
        }
        self.type_scope.push(scope);
        ids
    }

    // ----- pass 2: fields and variants ------------------------------------

    fn collect_adt_bodies(&mut self) {
        for (mi, m) in self.modules.iter().enumerate() {
            self.module = mi;
            for item in &m.items {
                match item {
                    Item::Struct(s) => {
                        let Some(TypeName::Adt(id)) = self.type_names.get(&s.name.name).copied()
                        else {
                            continue;
                        };
                        if self.t.adts[id as usize].span != s.span {
                            continue; // duplicate definition
                        }
                        let params = self.generic_scope(&s.generics);
                        self.self_type =
                            Some(Ty::Adt(id, params.iter().map(|p| Ty::Param(*p)).collect()));
                        let mut fields: Vec<FieldDef> = Vec::new();
                        for (fi, f) in s.fields.iter().enumerate() {
                            if fields.iter().any(|x| x.name == f.name.name) {
                                self.err(
                                    f.name.span,
                                    format!("field `{}` is declared twice", f.name.name),
                                );
                            }
                            let ty = self.resolve_type(&f.ty);
                            self.add_use(f.name.span, Def::Field(id, fi as u32), ty.clone());
                            fields.push(FieldDef {
                                name: f.name.name.clone(),
                                ty,
                                span: f.name.span,
                                doc: f.doc.clone(),
                            });
                        }
                        self.type_scope.pop();
                        self.self_type = None;
                        let adt = &mut self.t.adts[id as usize];
                        adt.params = params;
                        adt.kind = AdtKind::Struct(fields);
                    }
                    Item::Enum(e) => {
                        let Some(TypeName::Adt(id)) = self.type_names.get(&e.name.name).copied()
                        else {
                            continue;
                        };
                        if self.t.adts[id as usize].span != e.span {
                            continue;
                        }
                        let params = self.generic_scope(&e.generics);
                        let self_ty = Ty::Adt(id, params.iter().map(|p| Ty::Param(*p)).collect());
                        self.self_type = Some(self_ty.clone());
                        let mut variants: Vec<VariantDef> = Vec::new();
                        for (vi, v) in e.variants.iter().enumerate() {
                            if variants.iter().any(|x| x.name == v.name.name) {
                                self.err(
                                    v.name.span,
                                    format!("variant `{}` is declared twice", v.name.name),
                                );
                                continue;
                            }
                            let fields: Vec<Ty> =
                                v.fields.iter().map(|t| self.resolve_type(t)).collect();
                            let vty = if fields.is_empty() {
                                self_ty.clone()
                            } else {
                                Ty::func(fields.clone(), self_ty.clone())
                            };
                            self.add_use(v.name.span, Def::Variant(id, vi as u32), vty);
                            self.variants
                                .entry(v.name.name.clone())
                                .or_default()
                                .push((id, vi as u32));
                            variants.push(VariantDef {
                                name: v.name.name.clone(),
                                fields,
                                span: v.name.span,
                                doc: v.doc.clone(),
                            });
                        }
                        if variants.is_empty() {
                            self.err(e.name.span, "an enum needs at least one variant");
                        }
                        self.type_scope.pop();
                        self.self_type = None;
                        let adt = &mut self.t.adts[id as usize];
                        adt.params = params;
                        adt.kind = AdtKind::Enum(variants);
                    }
                    _ => {}
                }
            }
        }
        // Option and Result must have the layout the VM assumes.
        if let Some(id) = self.t.option_adt {
            let v = self.t.adts[id as usize].variants();
            assert!(v.len() == 2 && v[0].name == "Some" && v[1].name == "None");
        }
        if let Some(id) = self.t.result_adt {
            let v = self.t.adts[id as usize].variants();
            assert!(v.len() == 2 && v[0].name == "Ok" && v[1].name == "Err");
        }
    }

    // ----- types in source ---------------------------------------------------

    fn lookup_param(&self, name: &str) -> Option<ParamId> {
        self.type_scope
            .iter()
            .rev()
            .find_map(|s| s.get(name).copied())
    }

    fn resolve_type(&mut self, te: &TypeExpr) -> Ty {
        match &te.kind {
            TypeExprKind::Array(t) => Ty::array(self.resolve_type(t)),
            TypeExprKind::Tuple(ts) => {
                if ts.is_empty() {
                    Ty::Unit
                } else {
                    Ty::Tuple(ts.iter().map(|t| self.resolve_type(t)).collect())
                }
            }
            TypeExprKind::Fn(ps, r) => {
                let ps = ps.iter().map(|t| self.resolve_type(t)).collect();
                let r = r.as_ref().map(|t| self.resolve_type(t)).unwrap_or(Ty::Unit);
                Ty::func(ps, r)
            }
            TypeExprKind::Named(name, args) => {
                let args: Vec<Ty> = args.iter().map(|t| self.resolve_type(t)).collect();
                if let Some(p) = self.lookup_param(&name.name) {
                    if !args.is_empty() {
                        self.err(
                            te.span,
                            format!("type parameter `{}` takes no arguments", name.name),
                        );
                    }
                    self.add_use(name.span, Def::Param(p), Ty::Param(p));
                    return Ty::Param(p);
                }
                if name.name == "Self" {
                    if let Some(t) = self.self_type.clone() {
                        return t;
                    }
                    self.err(
                        name.span,
                        "`Self` can only be used inside `impl` blocks and type declarations",
                    );
                    return Ty::Error;
                }
                let Some(tn) = self.type_names.get(&name.name).copied() else {
                    let mut d = self.error(name.span, format!("cannot find type `{}`", name.name));
                    let candidates: Vec<String> = self.type_names.keys().cloned().collect();
                    if let Some(s) = suggest(&name.name, candidates.iter().map(|s| s.as_str())) {
                        d = d.with_label(format!("did you mean `{s}`?"));
                    }
                    self.emit(d);
                    return Ty::Error;
                };
                let expect_args = |n: usize, this: &mut Self| -> bool {
                    if args.len() != n {
                        this.err(
                            te.span,
                            format!(
                                "`{}` takes {} type argument{}, but {} {} given",
                                name.name,
                                n,
                                if n == 1 { "" } else { "s" },
                                args.len(),
                                if args.len() == 1 { "was" } else { "were" }
                            ),
                        );
                        false
                    } else {
                        true
                    }
                };
                match tn {
                    TypeName::Never => {
                        if !expect_args(0, self) {
                            return Ty::Error;
                        }
                        Ty::Never
                    }
                    TypeName::Int
                    | TypeName::Float
                    | TypeName::Bool
                    | TypeName::Str
                    | TypeName::Range => {
                        if !expect_args(0, self) {
                            return Ty::Error;
                        }
                        match tn {
                            TypeName::Int => Ty::Int,
                            TypeName::Float => Ty::Float,
                            TypeName::Bool => Ty::Bool,
                            TypeName::Str => Ty::Str,
                            _ => Ty::Range,
                        }
                    }
                    TypeName::Array => {
                        if !expect_args(1, self) {
                            return Ty::Error;
                        }
                        Ty::array(args[0].clone())
                    }
                    TypeName::Map => {
                        if !expect_args(2, self) {
                            return Ty::Error;
                        }
                        Ty::Map(Box::new(args[0].clone()), Box::new(args[1].clone()))
                    }
                    TypeName::Adt(id) => {
                        let n = self.t.adts[id as usize].params.len();
                        // Parameters are filled in during pass 2; until then
                        // the count is taken from the syntax.
                        let n = if n == 0 { self.declared_arity(id) } else { n };
                        if !expect_args(n, self) {
                            return Ty::Error;
                        }
                        self.add_use(name.span, Def::Adt(id), Ty::Unit);
                        Ty::Adt(id, args)
                    }
                }
            }
        }
    }

    fn declared_arity(&self, id: AdtId) -> usize {
        let name = &self.t.adts[id as usize].name;
        for m in self.modules {
            for item in &m.items {
                match item {
                    Item::Struct(s) if &s.name.name == name => return s.generics.len(),
                    Item::Enum(e) if &e.name.name == name => return e.generics.len(),
                    _ => {}
                }
            }
        }
        0
    }

    // ----- pass 3: signatures ------------------------------------------------

    fn collect_fns(&mut self) {
        for (mi, m) in self.modules.iter().enumerate() {
            self.module = mi;
            for (ii, item) in m.items.iter().enumerate() {
                match item {
                    Item::Fn(f) => {
                        if self.t.globals.contains_key(&f.name.name) {
                            self.err(
                                f.name.span,
                                format!("the function `{}` is defined more than once", f.name.name),
                            );
                            continue;
                        }
                        if f.self_param.is_some() {
                            self.err(
                                f.span,
                                "`self` parameters are only allowed in `impl` blocks",
                            );
                        }
                        let id = self.signature(
                            f,
                            None,
                            &[],
                            FnSource::Item {
                                module: mi,
                                item: ii,
                            },
                        );
                        self.t.globals.insert(f.name.name.clone(), id);
                    }
                    Item::Impl(imp) => self.collect_impl(imp, mi, ii),
                    Item::Test(t) => {
                        self.t.tests.push(TestDef {
                            name: t.name.clone(),
                            module: mi,
                            item: ii,
                            span: t.span,
                        });
                    }
                    _ => {}
                }
            }
        }
    }

    fn collect_impl(&mut self, imp: &ImplDecl, mi: usize, ii: usize) {
        let Some(tn) = self.type_names.get(&imp.target.name).copied() else {
            self.err(
                imp.target.span,
                format!("cannot find type `{}`", imp.target.name),
            );
            return;
        };
        let (con, arity) = match tn {
            TypeName::Int => (TypeCon::Int, 0),
            TypeName::Float => (TypeCon::Float, 0),
            TypeName::Bool => (TypeCon::Bool, 0),
            TypeName::Str => (TypeCon::Str, 0),
            TypeName::Array => (TypeCon::Array, 1),
            TypeName::Map => (TypeCon::Map, 2),
            TypeName::Range | TypeName::Never => {
                self.err(
                    imp.target.span,
                    format!("methods cannot be added to `{}`", imp.target.name),
                );
                return;
            }
            TypeName::Adt(id) => (TypeCon::Adt(id), self.t.adts[id as usize].params.len()),
        };
        if !self.is_prelude() && !matches!(con, TypeCon::Adt(_)) {
            self.err(
                imp.target.span,
                format!(
                    "methods can only be added to types declared in this program, not `{}`",
                    imp.target.name
                ),
            );
            return;
        }
        if let TypeCon::Adt(id) = con {
            self.add_use(imp.target.span, Def::Adt(id), Ty::Unit);
        }
        if imp.params.len() != arity {
            self.err(
                imp.target.span,
                format!(
                    "`{}` has {} type parameter{}; write `impl {}{}`",
                    imp.target.name,
                    arity,
                    if arity == 1 { "" } else { "s" },
                    imp.target.name,
                    if arity == 0 {
                        String::new()
                    } else {
                        let names: Vec<_> = ["T", "U", "V"].iter().take(arity).copied().collect();
                        format!("[{}]", names.join(", "))
                    }
                ),
            );
            return;
        }
        for (mi2, f) in imp.methods.iter().enumerate() {
            let key = (con, f.name.name.clone());
            if self.t.methods.contains_key(&key) {
                self.err(
                    f.name.span,
                    format!(
                        "`{}.{}` is defined more than once",
                        imp.target.name, f.name.name
                    ),
                );
                continue;
            }
            let id = self.signature(
                f,
                Some((con, &imp.target.name)),
                &imp.params,
                FnSource::Method {
                    module: mi,
                    item: ii,
                    method: mi2,
                },
            );
            self.t.methods.insert(key, id);
        }
    }

    fn self_ty_for(&self, con: TypeCon, params: &[ParamId]) -> Ty {
        let args: Vec<Ty> = params.iter().map(|p| Ty::Param(*p)).collect();
        match con {
            TypeCon::Int => Ty::Int,
            TypeCon::Float => Ty::Float,
            TypeCon::Bool => Ty::Bool,
            TypeCon::Str => Ty::Str,
            TypeCon::Array => Ty::array(args[0].clone()),
            TypeCon::Map => Ty::Map(Box::new(args[0].clone()), Box::new(args[1].clone())),
            TypeCon::Adt(id) => Ty::Adt(id, args),
        }
    }

    fn signature(
        &mut self,
        f: &FnDecl,
        owner: Option<(TypeCon, &str)>,
        impl_params: &[Ident],
        source: FnSource,
    ) -> FnId {
        // Each method gets its own copy of the impl's parameters, so `where`
        // clauses can bound them per method.
        let mut scope = HashMap::new();
        let mut generics = Vec::new();
        for p in impl_params {
            let bounds: Vec<Ident> = f
                .where_clauses
                .iter()
                .filter(|w| w.param.name == p.name)
                .flat_map(|w| w.bounds.clone())
                .collect();
            let id = self.new_param(p, &bounds);
            scope.insert(p.name.clone(), id);
            generics.push(id);
        }
        self.type_scope.push(scope);
        let own = self.generic_scope(&f.generics);
        // `where` clauses on the function's own parameters.
        for w in &f.where_clauses {
            if impl_params.iter().any(|p| p.name == w.param.name) {
                continue;
            }
            match own
                .iter()
                .find(|p| self.t.params[**p as usize].name == w.param.name)
            {
                Some(&p) => {
                    for b in &w.bounds {
                        match Bound::from_name(&b.name) {
                            Some(bound) => self.t.params[p as usize].bounds.push(bound),
                            None => self.err(b.span, format!("unknown bound `{}`", b.name)),
                        }
                    }
                }
                None => self.err(
                    w.param.span,
                    format!("`{}` is not a type parameter here", w.param.name),
                ),
            }
        }
        generics.extend(own);
        let self_ty = owner.map(|(con, _)| self.self_ty_for(con, &generics[..impl_params.len()]));
        self.self_type = self_ty.clone();
        let mut params = Vec::new();
        let mut names = Vec::new();
        if f.self_param.is_some() {
            params.push(self_ty.clone().unwrap_or(Ty::Error));
            names.push("self".to_string());
        }
        for p in &f.params {
            let ty =
                p.ty.as_ref()
                    .map(|t| self.resolve_type(t))
                    .unwrap_or(Ty::Error);
            params.push(ty);
            names.push(p.name.name.clone());
        }
        let ret = f
            .ret
            .as_ref()
            .map(|t| self.resolve_type(t))
            .unwrap_or(Ty::Unit);
        self.type_scope.pop();
        self.type_scope.pop();
        self.self_type = None;
        if f.body.is_none() && !self.is_prelude() {
            self.err(
                f.span,
                "`native` functions can only be declared in the prelude",
            );
        }
        let name = match owner {
            Some((_, target)) => format!("{}.{}", target, f.name.name),
            None => f.name.name.clone(),
        };
        let id = self.t.fns.len() as FnId;
        self.add_use(
            f.name.span,
            Def::Fn(id),
            Ty::func(params.clone(), ret.clone()),
        );
        self.t.fns.push(FnDef {
            name,
            short_name: f.name.name.clone(),
            owner: owner.map(|(c, _)| c),
            generics,
            has_self: f.self_param.is_some(),
            params,
            param_names: names,
            ret,
            native: f.body.is_none(),
            source,
            param_locals: Vec::new(),
            span: f.span,
            name_span: f.name.span,
            doc: f.doc.clone(),
            builtin: self.is_prelude(),
        });
        id
    }

    // ----- pass 4: bodies ----------------------------------------------------

    fn fn_decl(&self, source: FnSource) -> &'a FnDecl {
        match source {
            FnSource::Item { module, item } => match &self.modules[module].items[item] {
                Item::Fn(f) => f,
                _ => unreachable!(),
            },
            FnSource::Method {
                module,
                item,
                method,
            } => match &self.modules[module].items[item] {
                Item::Impl(i) => &i.methods[method],
                _ => unreachable!(),
            },
        }
    }

    fn check_bodies(&mut self) {
        for id in 0..self.t.fns.len() {
            let def = self.t.fns[id].clone();
            self.module = match def.source {
                FnSource::Item { module, .. } | FnSource::Method { module, .. } => module,
            };
            let decl = self.fn_decl(def.source);
            let mut scope = HashMap::new();
            for p in &def.generics {
                scope.insert(self.t.params[*p as usize].name.clone(), *p);
            }
            self.type_scope.push(scope);
            if let Some(con) = def.owner {
                let n_impl = def.generics.len() - decl.generics.len();
                self.self_type = Some(self.self_ty_for(con, &def.generics[..n_impl]));
            }
            self.ctxs.push(Ctx {
                ret: def.ret.clone(),
                scope_base: self.scopes.len(),
                captures: Vec::new(),
                loop_depth: 0,
            });
            self.scopes.push(HashMap::new());
            let mut locals = Vec::new();
            let mut offset = 0;
            if let Some(span) = decl.self_param {
                let l = self.declare("self", false, def.params[0].clone(), span);
                locals.push(l);
                offset = 1;
            }
            for (i, p) in decl.params.iter().enumerate() {
                if self.scopes.last().unwrap().contains_key(&p.name.name) {
                    self.err(
                        p.name.span,
                        format!("parameter `{}` is declared twice", p.name.name),
                    );
                }
                let l = self.declare(
                    &p.name.name,
                    false,
                    def.params[i + offset].clone(),
                    p.name.span,
                );
                locals.push(l);
            }
            self.t.fns[id].param_locals = locals;
            if let Some(body) = &decl.body {
                let ret = def.ret.clone();
                let ty = self.block(body, Some(&ret));
                let _ = ty;
            }
            self.scopes.pop();
            self.ctxs.pop();
            self.type_scope.pop();
            self.self_type = None;
        }
        let tests = self.t.tests.clone();
        for test in tests {
            self.module = test.module;
            let Item::Test(decl) = &self.modules[test.module].items[test.item] else {
                unreachable!()
            };
            self.ctxs.push(Ctx {
                ret: Ty::Unit,
                scope_base: self.scopes.len(),
                captures: Vec::new(),
                loop_depth: 0,
            });
            self.scopes.push(HashMap::new());
            self.block(&decl.body, Some(&Ty::Unit));
            self.scopes.pop();
            self.ctxs.pop();
        }
    }

    fn declare(&mut self, name: &str, mutable: bool, ty: Ty, span: Span) -> LocalId {
        let id = self.t.locals.len() as LocalId;
        self.t.locals.push(LocalDef {
            name: name.to_string(),
            mutable,
            ty: ty.clone(),
            span,
            captured: false,
            boxed: false,
        });
        self.scopes.last_mut().unwrap().insert(name.to_string(), id);
        self.local_ctx.insert(id, self.ctxs.len() - 1);
        self.add_use(span, Def::Local(id), ty);
        id
    }

    fn lookup_local(&mut self, name: &str) -> Option<LocalId> {
        let id = self
            .scopes
            .iter()
            .rev()
            .find_map(|s| s.get(name).copied())?;
        let def_ctx = self.local_ctx[&id];
        let cur = self.ctxs.len() - 1;
        if def_ctx < cur {
            self.t.locals[id as usize].captured = true;
            if self.t.locals[id as usize].mutable {
                self.t.locals[id as usize].boxed = true;
            }
            for ctx in &mut self.ctxs[def_ctx + 1..=cur] {
                if !ctx.captures.contains(&id) {
                    ctx.captures.push(id);
                }
            }
        }
        Some(id)
    }

    fn has_local(&self, name: &str) -> bool {
        self.scopes.iter().any(|s| s.contains_key(name))
    }

    fn ctx(&mut self) -> &mut Ctx {
        self.ctxs.last_mut().unwrap()
    }

    /// Instantiates a function's generics with fresh variables, adding
    /// obligations for their bounds. Returns (params, ret).
    fn instantiate(&mut self, id: FnId, span: Span) -> (Vec<Ty>, Ty) {
        let def = &self.t.fns[id as usize];
        let generics = def.generics.clone();
        let params = def.params.clone();
        let ret = def.ret.clone();
        let mut map = HashMap::new();
        for p in generics {
            let v = self.u.fresh();
            for b in self.t.params[p as usize].bounds.clone() {
                let fname = self.t.fns[id as usize].name.clone();
                self.obligations.push(Obligation {
                    ty: v.clone(),
                    kind: Oblig::Bound(b),
                    span,
                    what: format!("`{fname}`"),
                });
            }
            map.insert(p, v);
        }
        (
            params.iter().map(|t| t.subst(&map)).collect(),
            ret.subst(&map),
        )
    }

    fn instantiate_adt(&mut self, id: AdtId) -> (Vec<Ty>, HashMap<ParamId, Ty>) {
        let params = self.t.adts[id as usize].params.clone();
        let mut map = HashMap::new();
        let mut args = Vec::new();
        for p in params {
            let v = self.u.fresh();
            map.insert(p, v.clone());
            args.push(v);
        }
        (args, map)
    }

    fn oblige(&mut self, ty: &Ty, kind: Oblig, span: Span, what: &str) {
        self.obligations.push(Obligation {
            ty: ty.clone(),
            kind,
            span,
            what: what.to_string(),
        });
    }

    // ----- statements ------------------------------------------------------

    /// Checks a block. With `expected == Some(())` the final expression's
    /// value is discarded. Returns `Never` if the block cannot finish.
    fn block(&mut self, b: &Block, expected: Option<&Ty>) -> Ty {
        self.scopes.push(HashMap::new());
        let discard = matches!(expected.map(|t| self.u.shallow(t)), Some(Ty::Unit));
        let mut diverges = false;
        let mut result = None;
        let n = b.stmts.len();
        for (i, s) in b.stmts.iter().enumerate() {
            let last = i + 1 == n;
            match &s.kind {
                StmtKind::Expr(e) if last && !discard => {
                    let ty = self.expr(e, expected);
                    if self.u.shallow(&ty) == Ty::Never {
                        diverges = true;
                    }
                    result = Some(ty);
                }
                _ => {
                    if self.stmt(s) {
                        diverges = true;
                    }
                }
            }
        }
        self.scopes.pop();
        if diverges {
            return Ty::Never;
        }
        match result {
            Some(ty) => ty,
            None => {
                if let Some(exp) = expected {
                    if !discard && self.u.unify(exp, &Ty::Unit).is_err() {
                        let es = self.show(exp);
                        if !self.u.resolve(exp).has_error() {
                            let at = Span::new(b.span.hi.saturating_sub(1), b.span.hi);
                            let mut d = Diagnostic::error(at, "mismatched types")
                                .with_label(format!("expected `{es}`, found `()`"));
                            if let Some(last) = b.stmts.last() {
                                d = d.with_secondary(
                                    last.span,
                                    "the block ends with this statement, which has no value",
                                );
                            }
                            self.emit(d);
                        }
                    }
                }
                Ty::Unit
            }
        }
    }

    /// Checks a statement. Returns true if it never finishes.
    fn stmt(&mut self, s: &Stmt) -> bool {
        match &s.kind {
            StmtKind::Let {
                pattern,
                mutable,
                ty,
                init,
            } => {
                let declared = ty.as_ref().map(|t| self.resolve_type(t));
                let ty = match &declared {
                    Some(t) => {
                        self.expr(init, Some(t));
                        t.clone()
                    }
                    None => self.expr(init, None),
                };
                self.bind_irrefutable(pattern, &ty, *mutable, "`let`");
                self.u.shallow(&ty) == Ty::Never
            }
            StmtKind::Assign { target, op, value } => {
                self.assign(target, *op, value, s.span);
                false
            }
            StmtKind::While { cond, body } => {
                self.expr(cond, Some(&Ty::Bool));
                self.ctx().loop_depth += 1;
                self.block(body, Some(&Ty::Unit));
                self.ctx().loop_depth -= 1;
                false
            }
            StmtKind::For {
                pattern,
                iter,
                body,
            } => {
                let ity = self.expr(iter, None);
                let elem = match self.u.shallow(&ity) {
                    Ty::Range => Ty::Int,
                    Ty::Array(t) => *t,
                    Ty::Error => Ty::Error,
                    Ty::Map(..) => {
                        self.emit(
                            Diagnostic::error(iter.span, "a `Map` cannot be iterated directly")
                                .with_note("use `m.keys()`, `m.values()` or `m.entries()`"),
                        );
                        Ty::Error
                    }
                    Ty::Str => {
                        self.emit(
                            Diagnostic::error(iter.span, "a `String` cannot be iterated directly")
                                .with_note("use `s.chars()` for its characters"),
                        );
                        Ty::Error
                    }
                    Ty::Var(_) => {
                        self.err(iter.span, "the type of this value must be known here");
                        Ty::Error
                    }
                    other => {
                        let s = self.t.show(&other);
                        self.emit(
                            Diagnostic::error(iter.span, format!("`{s}` cannot be iterated"))
                                .with_note(
                                    "`for` loops work over arrays and ranges such as `0..n`",
                                ),
                        );
                        Ty::Error
                    }
                };
                self.record(iter.id, &ity);
                self.scopes.push(HashMap::new());
                self.bind_irrefutable(pattern, &elem, false, "`for`");
                self.ctx().loop_depth += 1;
                self.block(body, Some(&Ty::Unit));
                self.ctx().loop_depth -= 1;
                self.scopes.pop();
                false
            }
            StmtKind::Return(value) => {
                let ret = self.ctx().ret.clone();
                match value {
                    Some(v) => {
                        self.expr(v, Some(&ret));
                    }
                    None => {
                        if self.u.unify(&ret, &Ty::Unit).is_err() {
                            let r = self.show(&ret);
                            self.emit(
                                Diagnostic::error(s.span, "`return` needs a value here")
                                    .with_label(format!("the function returns `{r}`")),
                            );
                        }
                    }
                }
                true
            }
            StmtKind::Break | StmtKind::Continue => {
                if self.ctx().loop_depth == 0 {
                    let what = if matches!(s.kind, StmtKind::Break) {
                        "break"
                    } else {
                        "continue"
                    };
                    self.err(s.span, format!("`{what}` outside of a loop"));
                }
                true
            }
            StmtKind::Expr(e) => {
                let ty = self.expr(e, None);
                let ty = self.u.shallow(&ty);
                if !is_block_like(e) {
                    self.warn_unused(e, &ty);
                }
                ty == Ty::Never
            }
        }
    }

    fn warn_unused(&mut self, e: &Expr, ty: &Ty) {
        let pure = matches!(
            e.kind,
            ExprKind::Binary { .. }
                | ExprKind::Name(_)
                | ExprKind::Int(_)
                | ExprKind::Float(_)
                | ExprKind::Str(_)
                | ExprKind::Bool(_)
                | ExprKind::Field { .. }
                | ExprKind::Lambda { .. }
        );
        if pure && *ty != Ty::Unit && *ty != Ty::Error && *ty != Ty::Never {
            if let ExprKind::Binary { op: BinOp::Eq, .. } = e.kind {
                self.diags.push(
                    Diagnostic::warning(e.span, "unused comparison")
                        .with_note("to assign, use `=`"),
                );
                return;
            }
            self.diags
                .push(Diagnostic::warning(e.span, "this value is not used"));
        }
    }

    fn assign(&mut self, target: &Expr, op: AssignOp, value: &Expr, span: Span) {
        let tty = match &target.kind {
            ExprKind::Name(name) => match self.lookup_local(name) {
                Some(l) => {
                    let def = self.t.locals[l as usize].clone();
                    if !def.mutable {
                        self.emit(
                            Diagnostic::error(
                                target.span,
                                format!("cannot assign twice to `{name}`"),
                            )
                            .with_secondary(def.span, "declared with `let` here")
                            .with_note(format!(
                                "declare it with `var {name}` to make it reassignable"
                            )),
                        );
                    }
                    self.t.names.insert(target.id, Res::Local(l));
                    self.add_use(target.span, Def::Local(l), def.ty.clone());
                    self.record(target.id, &def.ty);
                    def.ty
                }
                None => {
                    if self.t.globals.contains_key(name) {
                        self.err(
                            target.span,
                            format!("cannot assign to the function `{name}`"),
                        );
                    } else {
                        self.err(target.span, format!("cannot find `{name}` in this scope"));
                    }
                    Ty::Error
                }
            },
            ExprKind::Field { .. } | ExprKind::Index { .. } => self.expr(target, None),
            ExprKind::TupleField { .. } => {
                self.expr(target, None);
                self.err(
                    target.span,
                    "tuple elements cannot be assigned; tuples are immutable",
                );
                Ty::Error
            }
            ExprKind::SelfValue => {
                self.err(target.span, "cannot assign to `self`");
                Ty::Error
            }
            _ => {
                self.expr(target, None);
                self.err(target.span, "this cannot be assigned to");
                Ty::Error
            }
        };
        self.expr(value, Some(&tty));
        match op {
            AssignOp::Set => {}
            AssignOp::Add => self.oblige(&tty, Oblig::Addable, span, "`+=`"),
            _ => self.oblige(&tty, Oblig::Num, span, "this compound assignment"),
        }
    }

    // ----- expressions -----------------------------------------------------

    fn expr(&mut self, e: &Expr, expected: Option<&Ty>) -> Ty {
        let ty = self.expr_inner(e, expected);
        let ty = match expected {
            Some(exp) if !self.handles_expected(e) => {
                if self.unify_at(e.span, exp, &ty) {
                    ty
                } else {
                    Ty::Error
                }
            }
            _ => ty,
        };
        self.record(e.id, &ty);
        ty
    }

    /// Expressions that push the expected type down to their parts and
    /// report mismatches there.
    fn handles_expected(&self, e: &Expr) -> bool {
        matches!(
            e.kind,
            ExprKind::If { .. } | ExprKind::Match { .. } | ExprKind::Block(_)
        )
    }

    fn expr_inner(&mut self, e: &Expr, expected: Option<&Ty>) -> Ty {
        match &e.kind {
            ExprKind::Int(_) => Ty::Int,
            ExprKind::Float(_) => Ty::Float,
            ExprKind::Str(_) => Ty::Str,
            ExprKind::Bool(_) => Ty::Bool,
            ExprKind::Unit => Ty::Unit,
            ExprKind::Error => Ty::Error,
            ExprKind::Interp(parts) => {
                for p in parts {
                    if let InterpPart::Expr(x) = p {
                        self.expr(x, None);
                    }
                }
                Ty::Str
            }
            ExprKind::Name(name) => self.name(e, name),
            ExprKind::SelfValue => match self.lookup_local("self") {
                Some(l) => {
                    self.t.names.insert(e.id, Res::Local(l));
                    let ty = self.t.locals[l as usize].ty.clone();
                    self.add_use(e.span, Def::Local(l), ty.clone());
                    ty
                }
                None => {
                    self.err(
                        e.span,
                        "`self` is only available in methods that take `self`",
                    );
                    Ty::Error
                }
            },
            ExprKind::Field { base, name } => self.field(e, base, name),
            ExprKind::TupleField {
                base,
                index,
                index_span,
            } => {
                let bty = self.expr(base, None);
                match self.u.shallow(&bty) {
                    Ty::Tuple(ts) => match ts.get(*index as usize) {
                        Some(t) => t.clone(),
                        None => {
                            self.err(
                                *index_span,
                                format!(
                                    "this tuple has {} elements; there is no `.{}`",
                                    ts.len(),
                                    index
                                ),
                            );
                            Ty::Error
                        }
                    },
                    Ty::Error => Ty::Error,
                    Ty::Var(_) => {
                        self.err(base.span, "the type of this value must be known here");
                        Ty::Error
                    }
                    other => {
                        let s = self.t.show(&other);
                        self.err(
                            *index_span,
                            format!("`.{index}` needs a tuple, but this is `{s}`"),
                        );
                        Ty::Error
                    }
                }
            }
            ExprKind::Call { callee, args } => self.call(e, callee, args),
            ExprKind::MethodCall { recv, name, args } => self.method_call(e, recv, name, args),
            ExprKind::Index { base, index } => {
                let bty = self.expr(base, None);
                match self.u.shallow(&bty) {
                    Ty::Array(t) => {
                        self.expr(index, Some(&Ty::Int));
                        *t
                    }
                    Ty::Map(k, v) => {
                        self.expr(index, Some(&k));
                        *v
                    }
                    Ty::Error => {
                        self.expr(index, None);
                        Ty::Error
                    }
                    Ty::Str => {
                        self.expr(index, None);
                        self.emit(
                            Diagnostic::error(e.span, "strings cannot be indexed").with_note(
                                "use `s.chars()`, `s.byte_at(i)` or `s.slice(start, end)`",
                            ),
                        );
                        Ty::Error
                    }
                    Ty::Var(_) => {
                        self.expr(index, None);
                        self.err(base.span, "the type of this value must be known here");
                        Ty::Error
                    }
                    other => {
                        self.expr(index, None);
                        let s = self.t.show(&other);
                        self.err(e.span, format!("`{s}` cannot be indexed"));
                        Ty::Error
                    }
                }
            }
            ExprKind::Binary { op, lhs, rhs } => self.binary(e, *op, lhs, rhs),
            ExprKind::Unary { op, expr } => match op {
                UnOp::Not => {
                    self.expr(expr, Some(&Ty::Bool));
                    Ty::Bool
                }
                UnOp::Neg => {
                    let t = self.expr(expr, None);
                    self.oblige(&t, Oblig::Num, e.span, "`-`");
                    t
                }
            },
            ExprKind::If { cond, then, else_ } => {
                self.expr(cond, Some(&Ty::Bool));
                match else_ {
                    None => {
                        self.block(then, Some(&Ty::Unit));
                        if let Some(exp) = expected {
                            if self.u.unify(exp, &Ty::Unit).is_err() {
                                let es = self.show(exp);
                                self.emit(
                                    Diagnostic::error(e.span, "`if` without `else` has no value")
                                        .with_label(format!("expected `{es}`"))
                                        .with_note("add an `else` branch"),
                                );
                            }
                        }
                        Ty::Unit
                    }
                    Some(els) => {
                        let ty = match expected {
                            Some(t) => t.clone(),
                            None => self.u.fresh(),
                        };
                        let t1 = self.block(then, Some(&ty));
                        let t2 = self.expr(els, Some(&ty));
                        if self.u.shallow(&t1) == Ty::Never && self.u.shallow(&t2) == Ty::Never {
                            Ty::Never
                        } else {
                            ty
                        }
                    }
                }
            }
            ExprKind::Match { scrutinee, arms } => self.match_expr(e, scrutinee, arms, expected),
            ExprKind::Block(b) => self.block(b, expected),
            ExprKind::Array(elems) => {
                let elem = match expected.map(|t| self.u.shallow(t)) {
                    Some(Ty::Array(t)) => *t,
                    _ => self.u.fresh(),
                };
                for x in elems {
                    self.expr(x, Some(&elem));
                }
                Ty::array(elem)
            }
            ExprKind::Tuple(elems) => {
                let hints = match expected.map(|t| self.u.shallow(t)) {
                    Some(Ty::Tuple(ts)) if ts.len() == elems.len() => Some(ts),
                    _ => None,
                };
                let mut tys = Vec::new();
                for (i, x) in elems.iter().enumerate() {
                    let h = hints.as_ref().map(|h| h[i].clone());
                    tys.push(self.expr(x, h.as_ref()));
                }
                Ty::Tuple(tys)
            }
            ExprKind::StructLit { name, fields } => self.struct_lit(e, name, fields),
            ExprKind::Lambda { params, ret, body } => {
                self.lambda(e, params, ret.as_ref(), body, expected)
            }
            ExprKind::Range { lo, hi, .. } => {
                self.expr(lo, Some(&Ty::Int));
                self.expr(hi, Some(&Ty::Int));
                Ty::Range
            }
            ExprKind::Try(inner) => self.try_expr(e, inner),
        }
    }

    fn name(&mut self, e: &Expr, name: &str) -> Ty {
        if let Some(l) = self.lookup_local(name) {
            self.t.names.insert(e.id, Res::Local(l));
            let ty = self.t.locals[l as usize].ty.clone();
            self.add_use(e.span, Def::Local(l), ty.clone());
            return ty;
        }
        if let Some(&f) = self.t.globals.get(name) {
            self.t.names.insert(e.id, Res::Fn(f));
            let (ps, r) = self.instantiate(f, e.span);
            let ty = Ty::func(ps, r);
            self.add_use(e.span, Def::Fn(f), ty.clone());
            return ty;
        }
        if let Some((adt, v)) = self.unique_variant(name, e.span) {
            return self.variant_value(e, adt, v);
        }
        if self.variants.contains_key(name) {
            return Ty::Error; // ambiguity already reported
        }
        if self.type_names.contains_key(name) {
            self.err(e.span, format!("`{name}` is a type, not a value"));
            return Ty::Error;
        }
        self.unknown_name(e.span, name);
        Ty::Error
    }

    fn unknown_name(&mut self, span: Span, name: &str) {
        let mut candidates: Vec<String> = Vec::new();
        for s in &self.scopes {
            candidates.extend(s.keys().cloned());
        }
        candidates.extend(
            self.t
                .globals
                .keys()
                .filter(|k| !k.starts_with('_'))
                .cloned(),
        );
        candidates.extend(self.variants.keys().cloned());
        let mut d = Diagnostic::error(span, format!("cannot find `{name}` in this scope"));
        if let Some(s) = suggest(name, candidates.iter().map(|s| s.as_str())) {
            d = d.with_label(format!("did you mean `{s}`?"));
        }
        self.emit(d);
    }

    fn unique_variant(&mut self, name: &str, span: Span) -> Option<(AdtId, u32)> {
        let cands = self.variants.get(name)?.clone();
        if cands.len() == 1 {
            return Some(cands[0]);
        }
        let enums: Vec<String> = cands
            .iter()
            .map(|(a, _)| format!("`{}.{}`", self.t.adts[*a as usize].name, name))
            .collect();
        self.emit(
            Diagnostic::error(span, format!("`{name}` is a variant of several enums"))
                .with_note(format!("write one of {}", enums.join(", "))),
        );
        None
    }

    fn variant_value(&mut self, e: &Expr, adt: AdtId, v: u32) -> Ty {
        self.t.names.insert(e.id, Res::Variant(adt, v));
        let (args, map) = self.instantiate_adt(adt);
        let fields: Vec<Ty> = self.t.adts[adt as usize].variants()[v as usize]
            .fields
            .iter()
            .map(|t| t.subst(&map))
            .collect();
        let ty = Ty::Adt(adt, args);
        let ty = if fields.is_empty() {
            ty
        } else {
            Ty::func(fields, ty)
        };
        let span = match &e.kind {
            ExprKind::Field { name, .. } => name.span,
            _ => e.span,
        };
        self.add_use(span, Def::Variant(adt, v), ty.clone());
        ty
    }

    /// `Name` used as the receiver of `.` refers to a type when it is not a
    /// local variable.
    fn type_receiver(&self, recv: &Expr) -> Option<TypeName> {
        match &recv.kind {
            ExprKind::Name(n) if !self.has_local(n) && !self.t.globals.contains_key(n) => {
                self.type_names.get(n).copied()
            }
            _ => None,
        }
    }

    fn field(&mut self, e: &Expr, base: &Expr, name: &Ident) -> Ty {
        if let Some(tn) = self.type_receiver(base) {
            if let TypeName::Adt(adt) = tn {
                self.add_use(base.span, Def::Adt(adt), Ty::Unit);
                self.record(base.id, &Ty::Unit);
                if let Some(v) = self.t.adts[adt as usize]
                    .variants()
                    .iter()
                    .position(|v| v.name == name.name)
                {
                    return self.variant_value(e, adt, v as u32);
                }
                if let Some(con) = self.con_of(tn) {
                    if self.t.methods.contains_key(&(con, name.name.clone())) {
                        self.err(
                            e.span,
                            format!(
                                "`{}.{}` is a function; call it with `(...)`",
                                self.t.adts[adt as usize].name, name.name
                            ),
                        );
                        return Ty::Error;
                    }
                }
                let tname = self.t.adts[adt as usize].name.clone();
                self.err(
                    name.span,
                    format!("`{tname}` has no variant or function named `{}`", name.name),
                );
                return Ty::Error;
            }
        }
        let bty = self.expr(base, None);
        match self.u.shallow(&bty) {
            Ty::Adt(adt, args) => {
                let def = &self.t.adts[adt as usize];
                if let Some(i) = def.fields().iter().position(|f| f.name == name.name) {
                    let map: HashMap<ParamId, Ty> = def.params.iter().copied().zip(args).collect();
                    let ty = def.fields()[i].ty.subst(&map);
                    self.t.fields.insert(e.id, i as u32);
                    self.add_use(name.span, Def::Field(adt, i as u32), ty.clone());
                    return ty;
                }
                let tname = def.name.clone();
                if self
                    .t
                    .methods
                    .contains_key(&(TypeCon::Adt(adt), name.name.clone()))
                {
                    self.err(
                        e.span,
                        format!(
                            "`{}` is a method; call it with `.{}()`",
                            name.name, name.name
                        ),
                    );
                    return Ty::Error;
                }
                let mut d =
                    Diagnostic::error(name.span, format!("`{tname}` has no field `{}`", name.name));
                let names: Vec<&str> = def.fields().iter().map(|f| f.name.as_str()).collect();
                if let Some(s) = suggest(&name.name, names.iter().copied()) {
                    d = d.with_label(format!("did you mean `{s}`?"));
                } else if !names.is_empty() {
                    d = d.with_note(format!("its fields are: {}", names.join(", ")));
                } else if def.is_enum() {
                    d = d.with_note("enums have no fields; use `match` to look inside");
                }
                self.emit(d);
                Ty::Error
            }
            Ty::Error => Ty::Error,
            Ty::Var(_) => {
                self.err(base.span, "the type of this value must be known here");
                Ty::Error
            }
            other => {
                let s = self.t.show(&other);
                let con = other.con();
                if con.is_some_and(|c| self.t.methods.contains_key(&(c, name.name.clone()))) {
                    self.err(
                        e.span,
                        format!(
                            "`{}` is a method; call it with `.{}()`",
                            name.name, name.name
                        ),
                    );
                } else {
                    self.err(name.span, format!("`{s}` has no field `{}`", name.name));
                }
                Ty::Error
            }
        }
    }

    fn con_of(&self, tn: TypeName) -> Option<TypeCon> {
        Some(match tn {
            TypeName::Int => TypeCon::Int,
            TypeName::Float => TypeCon::Float,
            TypeName::Bool => TypeCon::Bool,
            TypeName::Str => TypeCon::Str,
            TypeName::Array => TypeCon::Array,
            TypeName::Map => TypeCon::Map,
            TypeName::Adt(a) => TypeCon::Adt(a),
            TypeName::Range | TypeName::Never => return None,
        })
    }

    fn check_args(&mut self, call_span: Span, params: &[Ty], args: &[Expr], what: &str) {
        if params.len() != args.len() {
            let n = params.len();
            self.emit(Diagnostic::error(
                call_span,
                format!(
                    "{what} takes {n} argument{} but {} {} supplied",
                    if n == 1 { "" } else { "s" },
                    args.len(),
                    if args.len() == 1 { "was" } else { "were" }
                ),
            ));
            for a in args {
                self.expr(a, None);
            }
            return;
        }
        for (a, p) in args.iter().zip(params) {
            self.expr(a, Some(p));
        }
    }

    fn call(&mut self, e: &Expr, callee: &Expr, args: &[Expr]) -> Ty {
        if let ExprKind::Name(name) = &callee.kind {
            if !self.has_local(name) {
                if let Some(&f) = self.t.globals.get(name) {
                    let (ps, r) = self.instantiate(f, e.span);
                    self.t.names.insert(callee.id, Res::Fn(f));
                    self.t.calls.insert(e.id, CallRes::Direct(f));
                    self.add_use(callee.span, Def::Fn(f), Ty::func(ps.clone(), r.clone()));
                    self.record(callee.id, &Ty::func(ps.clone(), r.clone()));
                    self.check_args(e.span, &ps, args, &format!("`{name}`"));
                    return r;
                }
                if self.variants.contains_key(name) {
                    let Some((adt, v)) = self.unique_variant(name, callee.span) else {
                        for a in args {
                            self.expr(a, None);
                        }
                        return Ty::Error;
                    };
                    return self.construct_variant(e, callee, adt, v, args);
                }
                if let Some(TypeName::Adt(adt)) = self.type_names.get(name).copied() {
                    if !self.t.adts[adt as usize].is_enum() {
                        self.emit(
                            Diagnostic::error(
                                callee.span,
                                format!("`{name}` is a struct, not a function"),
                            )
                            .with_note(format!(
                                "construct it with `{name} {{ field: value, ... }}`"
                            )),
                        );
                        for a in args {
                            self.expr(a, None);
                        }
                        return Ty::Error;
                    }
                }
            }
        }
        let cty = self.expr(callee, None);
        self.t.calls.insert(e.id, CallRes::Indirect);
        self.call_value(e, callee.span, &cty, args)
    }

    fn call_value(&mut self, e: &Expr, callee_span: Span, cty: &Ty, args: &[Expr]) -> Ty {
        match self.u.shallow(cty) {
            Ty::Fn(ps, r) => {
                self.check_args(e.span, &ps, args, "this function");
                *r
            }
            Ty::Var(_) => {
                let ps: Vec<Ty> = args.iter().map(|_| self.u.fresh()).collect();
                let r = self.u.fresh();
                let _ = self.u.unify(cty, &Ty::func(ps.clone(), r.clone()));
                for (a, p) in args.iter().zip(&ps) {
                    self.expr(a, Some(p));
                }
                r
            }
            Ty::Error => {
                for a in args {
                    self.expr(a, None);
                }
                Ty::Error
            }
            other => {
                let s = self.t.show(&other);
                self.err(callee_span, format!("`{s}` is not a function"));
                for a in args {
                    self.expr(a, None);
                }
                Ty::Error
            }
        }
    }

    fn construct_variant(
        &mut self,
        e: &Expr,
        callee: &Expr,
        adt: AdtId,
        v: u32,
        args: &[Expr],
    ) -> Ty {
        let (targs, map) = self.instantiate_adt(adt);
        let def = &self.t.adts[adt as usize];
        let vdef = &def.variants()[v as usize];
        let fields: Vec<Ty> = vdef.fields.iter().map(|t| t.subst(&map)).collect();
        let vname = format!("{}.{}", def.name, vdef.name);
        let ty = Ty::Adt(adt, targs);
        self.t.calls.insert(e.id, CallRes::Variant(adt, v));
        self.t.names.insert(callee.id, Res::Variant(adt, v));
        let span = match &callee.kind {
            ExprKind::Field { name, .. } => name.span,
            _ => callee.span,
        };
        self.add_use(
            span,
            Def::Variant(adt, v),
            Ty::func(fields.clone(), ty.clone()),
        );
        if fields.is_empty() {
            self.emit(
                Diagnostic::error(e.span, format!("`{vname}` has no fields"))
                    .with_note(format!("write `{}` without parentheses", vdef_name(&vname))),
            );
            for a in args {
                self.expr(a, None);
            }
            return ty;
        }
        self.check_args(e.span, &fields, args, &format!("`{vname}`"));
        ty
    }

    fn method_call(&mut self, e: &Expr, recv: &Expr, name: &Ident, args: &[Expr]) -> Ty {
        if let Some(tn) = self.type_receiver(recv) {
            self.record(recv.id, &Ty::Unit);
            if let TypeName::Adt(adt) = tn {
                self.add_use(recv.span, Def::Adt(adt), Ty::Unit);
                if let Some(v) = self.t.adts[adt as usize]
                    .variants()
                    .iter()
                    .position(|v| v.name == name.name)
                {
                    let fake_callee = Expr {
                        id: recv.id,
                        kind: ExprKind::Field {
                            base: Box::new(recv.clone()),
                            name: name.clone(),
                        },
                        span: recv.span.to(name.span),
                    };
                    return self.construct_variant(e, &fake_callee, adt, v as u32, args);
                }
            }
            let tname = match &recv.kind {
                ExprKind::Name(n) => n.clone(),
                _ => unreachable!(),
            };
            let Some(con) = self.con_of(tn) else {
                self.err(e.span, format!("`{tname}` has no functions"));
                return Ty::Error;
            };
            let Some(&f) = self.t.methods.get(&(con, name.name.clone())) else {
                self.err(
                    name.span,
                    format!("`{tname}` has no function named `{}`", name.name),
                );
                for a in args {
                    self.expr(a, None);
                }
                return Ty::Error;
            };
            let (ps, r) = self.instantiate(f, e.span);
            self.add_use(name.span, Def::Fn(f), Ty::func(ps.clone(), r.clone()));
            if self.t.fns[f as usize].has_self {
                self.emit(
                    Diagnostic::error(
                        e.span,
                        format!("`{tname}.{}` is a method that takes `self`", name.name),
                    )
                    .with_note(format!("call it on a value: `x.{}(...)`", name.name)),
                );
                for a in args {
                    self.expr(a, None);
                }
                return r;
            }
            self.t.calls.insert(e.id, CallRes::Static(f));
            self.check_args(e.span, &ps, args, &format!("`{tname}.{}`", name.name));
            return r;
        }
        let rty = self.expr(recv, None);
        let r = self.u.shallow(&rty);
        let con = match &r {
            Ty::Error => {
                for a in args {
                    self.expr(a, None);
                }
                return Ty::Error;
            }
            Ty::Var(_) => {
                self.emit(
                    Diagnostic::error(recv.span, "the type of this value must be known here")
                        .with_note("add a type annotation, such as `let xs: [Int] = []`"),
                );
                for a in args {
                    self.expr(a, None);
                }
                return Ty::Error;
            }
            other => other.con(),
        };
        let method = con.and_then(|c| self.t.methods.get(&(c, name.name.clone())).copied());
        match method {
            Some(f) if self.t.fns[f as usize].has_self => {
                let (ps, ret) = self.instantiate(f, e.span);
                self.add_use(name.span, Def::Fn(f), Ty::func(ps.clone(), ret.clone()));
                self.u.unify(&ps[0], &rty).ok();
                self.t.calls.insert(e.id, CallRes::Direct(f));
                let fname = self.t.fns[f as usize].name.clone();
                self.check_args(e.span, &ps[1..], args, &format!("`{fname}`"));
                ret
            }
            Some(f) => {
                let fname = self.t.fns[f as usize].name.clone();
                self.err(
                    e.span,
                    format!("`{fname}` does not take `self`; call it as `{fname}(...)`"),
                );
                Ty::Error
            }
            None => {
                // A struct field holding a function: `obj.callback(x)`.
                if let Ty::Adt(adt, targs) = &r {
                    let def = &self.t.adts[*adt as usize];
                    if let Some(i) = def.fields().iter().position(|f| f.name == name.name) {
                        let map: HashMap<ParamId, Ty> =
                            def.params.iter().copied().zip(targs.clone()).collect();
                        let fty = def.fields()[i].ty.subst(&map);
                        self.t.calls.insert(e.id, CallRes::FieldClosure(i as u32));
                        self.add_use(name.span, Def::Field(*adt, i as u32), fty.clone());
                        return self.call_value(e, name.span, &fty, args);
                    }
                }
                let s = self.t.show(&r);
                let mut d = Diagnostic::error(
                    name.span,
                    format!("no method named `{}` on `{s}`", name.name),
                );
                if let Some(c) = con {
                    let names: Vec<String> = self
                        .t
                        .methods
                        .keys()
                        .filter(|(k, _)| *k == c)
                        .map(|(_, n)| n.clone())
                        .collect();
                    if let Some(sug) = suggest(&name.name, names.iter().map(|s| s.as_str())) {
                        d = d.with_label(format!("did you mean `{sug}`?"));
                    }
                }
                self.emit(d);
                for a in args {
                    self.expr(a, None);
                }
                Ty::Error
            }
        }
    }

    fn binary(&mut self, e: &Expr, op: BinOp, lhs: &Expr, rhs: &Expr) -> Ty {
        match op {
            BinOp::And | BinOp::Or => {
                self.expr(lhs, Some(&Ty::Bool));
                self.expr(rhs, Some(&Ty::Bool));
                Ty::Bool
            }
            BinOp::Eq | BinOp::Ne => {
                let l = self.expr(lhs, None);
                self.expr(rhs, Some(&l));
                if matches!(self.u.shallow(&l), Ty::Fn(..)) {
                    self.err(e.span, "functions cannot be compared");
                }
                Ty::Bool
            }
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let l = self.expr(lhs, None);
                self.expr(rhs, Some(&l));
                self.oblige(
                    &l,
                    Oblig::Bound(Bound::Ord),
                    e.span,
                    &format!("`{}`", op.symbol()),
                );
                Ty::Bool
            }
            BinOp::Add => {
                let l = self.expr(lhs, None);
                self.expr(rhs, Some(&l));
                self.oblige(&l, Oblig::Addable, e.span, "`+`");
                l
            }
            BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
                let l = self.expr(lhs, None);
                self.expr(rhs, Some(&l));
                self.oblige(&l, Oblig::Num, e.span, &format!("`{}`", op.symbol()));
                l
            }
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr => {
                self.expr(lhs, Some(&Ty::Int));
                self.expr(rhs, Some(&Ty::Int));
                Ty::Int
            }
        }
    }

    fn struct_lit(&mut self, e: &Expr, name: &Ident, fields: &[FieldInit]) -> Ty {
        let adt = match self.type_names.get(&name.name).copied() {
            Some(TypeName::Adt(a)) if !self.t.adts[a as usize].is_enum() => a,
            Some(TypeName::Adt(_)) => {
                self.emit(
                    Diagnostic::error(
                        name.span,
                        format!("`{}` is an enum, not a struct", name.name),
                    )
                    .with_note("construct one of its variants instead"),
                );
                for f in fields {
                    self.expr(&f.value, None);
                }
                return Ty::Error;
            }
            Some(_) => {
                self.err(name.span, format!("`{}` is not a struct", name.name));
                for f in fields {
                    self.expr(&f.value, None);
                }
                return Ty::Error;
            }
            None => {
                self.err(name.span, format!("cannot find struct `{}`", name.name));
                for f in fields {
                    self.expr(&f.value, None);
                }
                return Ty::Error;
            }
        };
        self.add_use(name.span, Def::Adt(adt), Ty::Unit);
        let (args, map) = self.instantiate_adt(adt);
        let defs = self.t.adts[adt as usize].fields().to_vec();
        let mut seen = HashSet::new();
        for f in fields {
            match defs.iter().position(|d| d.name == f.name.name) {
                Some(i) => {
                    if !seen.insert(i) {
                        self.err(
                            f.name.span,
                            format!("field `{}` is given twice", f.name.name),
                        );
                    }
                    let ty = defs[i].ty.subst(&map);
                    self.add_use(f.name.span, Def::Field(adt, i as u32), ty.clone());
                    self.expr(&f.value, Some(&ty));
                }
                None => {
                    let mut d = Diagnostic::error(
                        f.name.span,
                        format!("`{}` has no field `{}`", name.name, f.name.name),
                    );
                    if let Some(s) = suggest(&f.name.name, defs.iter().map(|d| d.name.as_str())) {
                        d = d.with_label(format!("did you mean `{s}`?"));
                    }
                    self.emit(d);
                    self.expr(&f.value, None);
                }
            }
        }
        let missing: Vec<String> = defs
            .iter()
            .enumerate()
            .filter(|(i, _)| !seen.contains(i))
            .map(|(_, d)| format!("`{}`", d.name))
            .collect();
        if !missing.is_empty() {
            self.err(
                e.span,
                format!(
                    "missing field{} {} in `{}`",
                    if missing.len() == 1 { "" } else { "s" },
                    missing.join(", "),
                    name.name
                ),
            );
        }
        Ty::Adt(adt, args)
    }

    fn lambda(
        &mut self,
        e: &Expr,
        params: &[Param],
        ret: Option<&TypeExpr>,
        body: &Expr,
        expected: Option<&Ty>,
    ) -> Ty {
        let hint = match expected.map(|t| self.u.shallow(t)) {
            Some(Ty::Fn(ps, r)) if ps.len() == params.len() => Some((ps, *r)),
            _ => None,
        };
        let mut ptys = Vec::new();
        for (i, p) in params.iter().enumerate() {
            let ty = match (&p.ty, &hint) {
                (Some(t), Some((hs, _))) => {
                    let t = self.resolve_type(t);
                    self.unify_at(p.name.span, &hs[i], &t);
                    t
                }
                (Some(t), None) => self.resolve_type(t),
                (None, Some((hs, _))) => hs[i].clone(),
                (None, None) => self.u.fresh(),
            };
            ptys.push(ty);
        }
        let ret_ty = match (ret, &hint) {
            (Some(t), _) => self.resolve_type(t),
            (None, Some((_, r))) => r.clone(),
            (None, None) => self.u.fresh(),
        };
        self.ctxs.push(Ctx {
            ret: ret_ty.clone(),
            scope_base: self.scopes.len(),
            captures: Vec::new(),
            loop_depth: 0,
        });
        self.scopes.push(HashMap::new());
        let mut locals = Vec::new();
        for (p, ty) in params.iter().zip(&ptys) {
            locals.push(self.declare(&p.name.name, false, ty.clone(), p.name.span));
        }
        self.expr(body, Some(&ret_ty));
        self.scopes.pop();
        let ctx = self.ctxs.pop().unwrap();
        let _ = ctx.scope_base;
        self.t.lambdas.insert(
            e.id,
            LambdaInfo {
                params: locals,
                captures: ctx.captures,
            },
        );
        Ty::func(ptys, ret_ty)
    }

    fn try_expr(&mut self, e: &Expr, inner: &Expr) -> Ty {
        let ty = self.expr(inner, None);
        let ret = self.ctx().ret.clone();
        let opt = self.t.option_adt;
        let res = self.t.result_adt;
        match self.u.shallow(&ty) {
            Ty::Adt(id, args) if Some(id) == opt => {
                let want = Ty::Adt(id, vec![self.u.fresh()]);
                if self.u.unify(&ret, &want).is_err() {
                    let r = self.show(&ret);
                    self.emit(
                        Diagnostic::error(
                            e.span,
                            "`?` on an `Option` needs the function to return an `Option`",
                        )
                        .with_label(format!("this function returns `{r}`")),
                    );
                }
                args[0].clone()
            }
            Ty::Adt(id, args) if Some(id) == res => {
                let want = Ty::Adt(id, vec![self.u.fresh(), args[1].clone()]);
                if self.u.unify(&ret, &want).is_err() {
                    let r = self.show(&ret);
                    let es = self.show(&args[1]);
                    self.emit(
                        Diagnostic::error(e.span, "`?` on a `Result` needs the function to return a `Result` with the same error type")
                            .with_label(format!("this function returns `{r}`, but the error type here is `{es}`")),
                    );
                }
                args[0].clone()
            }
            Ty::Error => Ty::Error,
            Ty::Var(_) => {
                self.err(inner.span, "the type of this value must be known here");
                Ty::Error
            }
            other => {
                let s = self.t.show(&other);
                self.err(
                    e.span,
                    format!("`?` works on `Option` and `Result` values, not `{s}`"),
                );
                Ty::Error
            }
        }
    }

    // ----- patterns and match ------------------------------------------------

    fn match_expr(
        &mut self,
        e: &Expr,
        scrutinee: &Expr,
        arms: &[Arm],
        expected: Option<&Ty>,
    ) -> Ty {
        let sty = self.expr(scrutinee, None);
        let ty = match expected {
            Some(t) => t.clone(),
            None => self.u.fresh(),
        };
        let mut all_never = !arms.is_empty();
        for arm in arms {
            self.scopes.push(HashMap::new());
            let mut binds = Vec::new();
            self.pattern(&arm.pattern, &sty, &mut binds, None);
            self.declare_binds(&binds, false);
            if let Some(g) = &arm.guard {
                self.expr(g, Some(&Ty::Bool));
            }
            let t = self.expr(&arm.body, Some(&ty));
            if self.u.shallow(&t) != Ty::Never {
                all_never = false;
            }
            self.scopes.pop();
        }
        self.check_exhaustive(e, &sty, arms);
        if all_never {
            Ty::Never
        } else {
            ty
        }
    }

    fn declare_binds(&mut self, binds: &[(String, Ty, Span, NodeId)], mutable: bool) {
        for (name, ty, span, pid) in binds {
            let l = self.declare(name, mutable, ty.clone(), *span);
            self.t.patterns.insert(*pid, PatRes::Bind(l));
        }
    }

    fn bind_irrefutable(&mut self, p: &Pattern, ty: &Ty, mutable: bool, what: &str) {
        let mut binds = Vec::new();
        self.pattern(p, ty, &mut binds, None);
        self.declare_binds(&binds, mutable);
        let rty = self.u.resolve(ty);
        if rty.has_error() {
            return;
        }
        let row = self.lower_pattern(p);
        let resolver = |t: &Ty| self.u.resolve(t);
        if let Some(w) = exhaust::missing(&self.t.adts, &resolver, &[vec![row]], &rty) {
            let text = exhaust::show(&self.t.adts, &w);
            self.emit(
                Diagnostic::error(
                    p.span,
                    format!("this pattern does not match every value in {what}"),
                )
                .with_label(format!("`{text}` is not covered"))
                .with_note("use `match` to handle the other cases"),
            );
        }
    }

    /// Checks a pattern against `ty`, collecting the names it binds. For the
    /// second and later alternatives of an or-pattern, `existing` holds the
    /// first alternative's bindings, which every alternative must repeat.
    fn pattern(
        &mut self,
        p: &Pattern,
        ty: &Ty,
        binds: &mut Vec<(String, Ty, Span, NodeId)>,
        existing: Option<&[(String, Ty, Span, NodeId)]>,
    ) {
        self.t.pattern_types.insert(p.id, ty.clone());
        match &p.kind {
            PatternKind::Wild | PatternKind::Error => {}
            PatternKind::Int(_) => {
                self.unify_at(p.span, ty, &Ty::Int);
            }
            PatternKind::Str(_) => {
                self.unify_at(p.span, ty, &Ty::Str);
            }
            PatternKind::Bool(_) => {
                self.unify_at(p.span, ty, &Ty::Bool);
            }
            PatternKind::Name(name) => {
                // A variant without fields if one fits, otherwise a binding.
                if let Some(cands) = self.variants.get(name).cloned() {
                    let fitting: Vec<(AdtId, u32)> = match self.u.shallow(ty) {
                        Ty::Adt(id, _) => cands.iter().copied().filter(|(a, _)| *a == id).collect(),
                        _ => cands.clone(),
                    };
                    if fitting.len() == 1 {
                        let (adt, v) = fitting[0];
                        let nfields = self.t.adts[adt as usize].variants()[v as usize]
                            .fields
                            .len();
                        if nfields > 0 {
                            let blanks = vec!["_"; nfields].join(", ");
                            self.emit(
                                Diagnostic::error(
                                    p.span,
                                    format!("the variant `{name}` has fields"),
                                )
                                .with_note(format!("match it with `{name}({blanks})`")),
                            );
                            return;
                        }
                        let (args, _) = self.instantiate_adt(adt);
                        self.unify_at(p.span, ty, &Ty::Adt(adt, args.clone()));
                        self.t.patterns.insert(p.id, PatRes::Variant(adt, v));
                        self.add_use(p.span, Def::Variant(adt, v), Ty::Adt(adt, args));
                        return;
                    }
                    if fitting.len() > 1 {
                        self.err(p.span, format!("`{name}` is a variant of several enums; qualify it as `Enum.{name}`"));
                        return;
                    }
                }
                if binds.iter().any(|(n, ..)| n == name) {
                    self.err(
                        p.span,
                        format!("`{name}` is bound more than once in this pattern"),
                    );
                    return;
                }
                if let Some(ex) = existing {
                    match ex.iter().find(|(n, ..)| n == name) {
                        Some((_, ety, _, _)) => {
                            let ety = ety.clone();
                            self.unify_at(p.span, &ety, ty);
                        }
                        None => {
                            self.err(p.span, format!("`{name}` is not bound in the first alternative of this `|` pattern"));
                        }
                    }
                }
                binds.push((name.clone(), ty.clone(), p.span, p.id));
            }
            PatternKind::Variant {
                enum_name,
                name,
                args,
            } => {
                let found = match enum_name {
                    Some(en) => match self.type_names.get(&en.name).copied() {
                        Some(TypeName::Adt(a)) => {
                            self.add_use(en.span, Def::Adt(a), Ty::Unit);
                            match self.t.adts[a as usize]
                                .variants()
                                .iter()
                                .position(|v| v.name == name.name)
                            {
                                Some(v) => Some((a, v as u32)),
                                None => {
                                    self.err(
                                        name.span,
                                        format!("`{}` has no variant `{}`", en.name, name.name),
                                    );
                                    None
                                }
                            }
                        }
                        _ => {
                            self.err(en.span, format!("cannot find enum `{}`", en.name));
                            None
                        }
                    },
                    None => match self.variants.get(&name.name).cloned() {
                        Some(cands) => {
                            let fitting: Vec<_> = match self.u.shallow(ty) {
                                Ty::Adt(id, _) => {
                                    cands.iter().copied().filter(|(a, _)| *a == id).collect()
                                }
                                _ => cands.clone(),
                            };
                            match fitting.len() {
                                1 => Some(fitting[0]),
                                0 => Some(cands[0]),
                                _ => {
                                    self.err(name.span, format!("`{}` is a variant of several enums; qualify it as `Enum.{}`", name.name, name.name));
                                    None
                                }
                            }
                        }
                        None => {
                            if let Some(TypeName::Adt(a)) = self.type_names.get(&name.name).copied()
                            {
                                if !self.t.adts[a as usize].is_enum() {
                                    self.emit(
                                        Diagnostic::error(
                                            name.span,
                                            format!("`{}` is a struct", name.name),
                                        )
                                        .with_note(
                                            format!(
                                                "match it with `{} {{ field, .. }}`",
                                                name.name
                                            ),
                                        ),
                                    );
                                }
                            } else {
                                self.err(name.span, format!("cannot find variant `{}`", name.name));
                            }
                            None
                        }
                    },
                };
                let Some((adt, v)) = found else {
                    for a in args {
                        self.pattern(a, &Ty::Error, binds, existing);
                    }
                    return;
                };
                let (targs, map) = self.instantiate_adt(adt);
                let aty = Ty::Adt(adt, targs);
                self.unify_at(p.span, ty, &aty);
                self.t.patterns.insert(p.id, PatRes::Variant(adt, v));
                self.add_use(name.span, Def::Variant(adt, v), aty);
                let fields: Vec<Ty> = self.t.adts[adt as usize].variants()[v as usize]
                    .fields
                    .iter()
                    .map(|t| t.subst(&map))
                    .collect();
                if fields.len() != args.len() {
                    let vname = &self.t.adts[adt as usize].variants()[v as usize].name;
                    self.err(
                        p.span,
                        format!(
                            "`{vname}` has {} field{}, but the pattern has {}",
                            fields.len(),
                            if fields.len() == 1 { "" } else { "s" },
                            args.len()
                        ),
                    );
                    for a in args {
                        self.pattern(a, &Ty::Error, binds, existing);
                    }
                    return;
                }
                for (a, f) in args.iter().zip(&fields) {
                    self.pattern(a, f, binds, existing);
                }
            }
            PatternKind::Struct { name, fields, rest } => {
                let adt = match self.type_names.get(&name.name).copied() {
                    Some(TypeName::Adt(a)) if !self.t.adts[a as usize].is_enum() => a,
                    _ => {
                        self.err(name.span, format!("cannot find struct `{}`", name.name));
                        for (_, fp) in fields {
                            self.pattern(fp, &Ty::Error, binds, existing);
                        }
                        return;
                    }
                };
                let (targs, map) = self.instantiate_adt(adt);
                self.unify_at(p.span, ty, &Ty::Adt(adt, targs));
                self.t.patterns.insert(p.id, PatRes::Struct(adt));
                self.add_use(name.span, Def::Adt(adt), Ty::Unit);
                let defs = self.t.adts[adt as usize].fields().to_vec();
                let mut seen = HashSet::new();
                for (fname, fp) in fields {
                    match defs.iter().position(|d| d.name == fname.name) {
                        Some(i) => {
                            seen.insert(i);
                            let fty = defs[i].ty.subst(&map);
                            self.add_use(fname.span, Def::Field(adt, i as u32), fty.clone());
                            self.pattern(fp, &fty, binds, existing);
                        }
                        None => {
                            self.err(
                                fname.span,
                                format!("`{}` has no field `{}`", name.name, fname.name),
                            );
                            self.pattern(fp, &Ty::Error, binds, existing);
                        }
                    }
                }
                if !rest && seen.len() < defs.len() {
                    let missing: Vec<String> = defs
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| !seen.contains(i))
                        .map(|(_, d)| format!("`{}`", d.name))
                        .collect();
                    self.emit(
                        Diagnostic::error(
                            p.span,
                            format!("pattern does not mention {}", missing.join(", ")),
                        )
                        .with_note("add `..` to ignore the other fields"),
                    );
                }
            }
            PatternKind::Tuple(elems) => {
                if elems.is_empty() {
                    self.unify_at(p.span, ty, &Ty::Unit);
                    return;
                }
                let tys: Vec<Ty> = match self.u.shallow(ty) {
                    Ty::Tuple(ts) if ts.len() == elems.len() => ts,
                    _ => {
                        let fresh: Vec<Ty> = elems.iter().map(|_| self.u.fresh()).collect();
                        self.unify_at(p.span, ty, &Ty::Tuple(fresh.clone()));
                        fresh
                    }
                };
                for (x, t) in elems.iter().zip(&tys) {
                    self.pattern(x, t, binds, existing);
                }
            }
            PatternKind::Or(alts) => {
                let start = binds.len();
                self.pattern(&alts[0], ty, binds, existing);
                let first: Vec<_> = binds[start..].to_vec();
                for alt in &alts[1..] {
                    let mut other = Vec::new();
                    self.pattern(alt, ty, &mut other, Some(&first));
                    for (name, ..) in &first {
                        if !other.iter().any(|(n, ..)| n == name) {
                            self.err(alt.span, format!("`{name}` must be bound in every alternative of this `|` pattern"));
                        }
                    }
                    // Later alternatives bind the same locals as the first.
                    for (name, _, _, pid) in other {
                        if let Some(idx) = first.iter().position(|(n, ..)| *n == name) {
                            self.t.pattern_types.insert(pid, first[idx].1.clone());
                            pending_alias(&mut self.t, pid, first[idx].3);
                        }
                    }
                }
            }
        }
    }

    // ----- exhaustiveness ----------------------------------------------------

    fn lower_pattern(&self, p: &Pattern) -> Pat {
        match &p.kind {
            PatternKind::Wild | PatternKind::Error => Pat::Wild,
            PatternKind::Int(v) => Pat::Ctor(Ctor::Int(*v), vec![]),
            PatternKind::Str(s) => Pat::Ctor(Ctor::Str(s.clone()), vec![]),
            PatternKind::Bool(b) => Pat::Ctor(Ctor::Bool(*b), vec![]),
            PatternKind::Name(_) => match self.t.patterns.get(&p.id) {
                Some(PatRes::Variant(a, v)) => Pat::Ctor(Ctor::Variant(*a, *v), vec![]),
                _ => Pat::Wild,
            },
            PatternKind::Variant { args, .. } => match self.t.patterns.get(&p.id) {
                Some(PatRes::Variant(a, v)) => {
                    let n = self.t.adts[*a as usize].variants()[*v as usize]
                        .fields
                        .len();
                    if n != args.len() {
                        return Pat::Wild;
                    }
                    Pat::Ctor(
                        Ctor::Variant(*a, *v),
                        args.iter().map(|x| self.lower_pattern(x)).collect(),
                    )
                }
                _ => Pat::Wild,
            },
            PatternKind::Struct { fields, .. } => match self.t.patterns.get(&p.id) {
                Some(PatRes::Struct(a)) => {
                    let defs = self.t.adts[*a as usize].fields();
                    let mut args = vec![Pat::Wild; defs.len()];
                    for (fname, fp) in fields {
                        if let Some(i) = defs.iter().position(|d| d.name == fname.name) {
                            args[i] = self.lower_pattern(fp);
                        }
                    }
                    Pat::Ctor(Ctor::Struct(*a), args)
                }
                _ => Pat::Wild,
            },
            PatternKind::Tuple(elems) => Pat::Ctor(
                Ctor::Tuple(elems.len()),
                elems.iter().map(|x| self.lower_pattern(x)).collect(),
            ),
            PatternKind::Or(alts) => Pat::Or(alts.iter().map(|x| self.lower_pattern(x)).collect()),
        }
    }

    fn check_exhaustive(&mut self, e: &Expr, sty: &Ty, arms: &[Arm]) {
        let sty = self.u.resolve(sty);
        if sty.has_error() {
            return;
        }
        let mut rows: Vec<Vec<Pat>> = Vec::new();
        let mut warnings = Vec::new();
        {
            let resolver = |t: &Ty| self.u.resolve(t);
            for arm in arms {
                let pat = self.lower_pattern(&arm.pattern);
                let alts: Vec<(Pat, Span)> = match (&pat, &arm.pattern.kind) {
                    (Pat::Or(ps), PatternKind::Or(asts)) => ps
                        .iter()
                        .cloned()
                        .zip(asts.iter().map(|a| a.span))
                        .collect(),
                    _ => vec![(pat.clone(), arm.pattern.span)],
                };
                for (alt, span) in alts {
                    if exhaust::useful(
                        &self.t.adts,
                        &resolver,
                        &rows,
                        std::slice::from_ref(&alt),
                        std::slice::from_ref(&sty),
                    )
                    .is_none()
                    {
                        warnings.push(span);
                    } else if arm.guard.is_none() {
                        rows.push(vec![alt]);
                    }
                }
            }
            if let Some(w) = exhaust::missing(&self.t.adts, &resolver, &rows, &sty) {
                let text = exhaust::show(&self.t.adts, &w);
                let span = match &e.kind {
                    ExprKind::Match { scrutinee, .. } => scrutinee.span,
                    _ => e.span,
                };
                let mut d = Diagnostic::error(span, "this `match` does not handle every case")
                    .with_label(format!("`{text}` is not covered"));
                if arms.iter().any(|a| a.guard.is_some()) {
                    d = d.with_note("arms with `if` guards do not count towards covering a case");
                }
                self.diags.push(d);
            }
        }
        for span in warnings {
            self.diags.push(
                Diagnostic::warning(span, "unreachable pattern")
                    .with_label("earlier arms already match every value this would"),
            );
        }
    }

    // ----- obligations and final types -----------------------------------------

    fn solve_obligations(&mut self) {
        let obligations = std::mem::take(&mut self.obligations);
        for ob in &obligations {
            let ty = self.u.shallow(&ob.ty);
            if let Ty::Var(_) = ty {
                // Nothing pinned it down; numbers default to Int.
                let _ = self.u.unify(&ty, &Ty::Int);
            }
        }
        for ob in obligations {
            let ty = self.u.resolve(&ob.ty);
            if ty.has_error() || ty == Ty::Never {
                continue;
            }
            if let Err(msg) = self.satisfies(&ty, ob.kind) {
                let s = self.t.show(&ty);
                let mut d = match ob.kind {
                    Oblig::Num => Diagnostic::error(
                        ob.span,
                        format!("{} needs `Int` or `Float`, found `{s}`", ob.what),
                    ),
                    Oblig::Addable => Diagnostic::error(
                        ob.span,
                        format!("{} needs `Int`, `Float` or `String`, found `{s}`", ob.what),
                    ),
                    Oblig::Bound(b) => Diagnostic::error(
                        ob.span,
                        format!(
                            "{} needs a type that is `{}`, found `{s}`",
                            ob.what,
                            b.name()
                        ),
                    ),
                };
                if let Some(m) = msg {
                    d = d.with_note(m);
                }
                self.diags.push(d);
            }
        }
    }

    fn satisfies(&self, ty: &Ty, kind: Oblig) -> Result<(), Option<String>> {
        let param_has = |p: ParamId, b: Bound| self.t.params[p as usize].bounds.contains(&b);
        match kind {
            Oblig::Num | Oblig::Bound(Bound::Num) => match ty {
                Ty::Int | Ty::Float => Ok(()),
                Ty::Param(p) if param_has(*p, Bound::Num) => Ok(()),
                Ty::Param(p) => Err(Some(format!(
                    "declare the bound on the type parameter: `{}: Num`",
                    self.t.params[*p as usize].name
                ))),
                _ => Err(None),
            },
            Oblig::Addable => match ty {
                Ty::Int | Ty::Float | Ty::Str => Ok(()),
                Ty::Param(p) if param_has(*p, Bound::Num) => Ok(()),
                Ty::Param(p) => Err(Some(format!(
                    "declare the bound on the type parameter: `{}: Num`",
                    self.t.params[*p as usize].name
                ))),
                _ => Err(None),
            },
            Oblig::Bound(Bound::Ord) => match ty {
                Ty::Int | Ty::Float | Ty::Str => Ok(()),
                Ty::Tuple(ts) => {
                    for t in ts {
                        self.satisfies(t, kind)?;
                    }
                    Ok(())
                }
                Ty::Array(t) => self.satisfies(t, kind),
                Ty::Param(p) if param_has(*p, Bound::Ord) => Ok(()),
                Ty::Param(p) => Err(Some(format!(
                    "declare the bound on the type parameter: `{}: Ord`",
                    self.t.params[*p as usize].name
                ))),
                _ => Err(Some(
                    "`Int`, `Float`, `String`, and tuples and arrays of them are `Ord`".into(),
                )),
            },
            Oblig::Bound(Bound::Key) => match ty {
                Ty::Int | Ty::Str | Ty::Bool => Ok(()),
                Ty::Param(p) if param_has(*p, Bound::Key) => Ok(()),
                Ty::Param(p) => Err(Some(format!(
                    "declare the bound on the type parameter: `{}: Key`",
                    self.t.params[*p as usize].name
                ))),
                _ => Err(Some("map keys must be `Int`, `String` or `Bool`".into())),
            },
        }
    }

    fn finish(&mut self) {
        let u = &self.u;
        let fix = |t: &Ty| -> Ty {
            let r = u.resolve(t);
            default_vars(&r)
        };
        for t in self.t.expr_types.values_mut() {
            *t = fix(t);
        }
        for t in self.t.pattern_types.values_mut() {
            *t = fix(t);
        }
        for l in &mut self.t.locals {
            l.ty = fix(&l.ty);
        }
        for u in &mut self.t.uses {
            u.ty = fix(&u.ty);
        }
        // Later or-pattern alternatives share the first alternative's local.
        let aliases: Vec<(NodeId, NodeId)> = std::mem::take(&mut self.t.alias_patterns);
        for (pid, first) in aliases {
            if let Some(r) = self.t.patterns.get(&first).copied() {
                self.t.patterns.insert(pid, r);
            }
        }
        self.diags
            .sort_by_key(|d| (d.span.lo, d.severity == sedge_syntax::Severity::Warning));
    }
}

fn pending_alias(t: &mut Typed, pid: NodeId, first: NodeId) {
    t.alias_patterns.push((pid, first));
}

/// Unsolved variables that nothing constrained become `()`.
fn default_vars(t: &Ty) -> Ty {
    match t {
        Ty::Var(_) => Ty::Unit,
        Ty::Array(x) => Ty::array(default_vars(x)),
        Ty::Map(k, v) => Ty::Map(Box::new(default_vars(k)), Box::new(default_vars(v))),
        Ty::Tuple(ts) => Ty::Tuple(ts.iter().map(default_vars).collect()),
        Ty::Adt(id, ts) => Ty::Adt(*id, ts.iter().map(default_vars).collect()),
        Ty::Fn(ps, r) => Ty::Fn(
            ps.iter().map(default_vars).collect(),
            Box::new(default_vars(r)),
        ),
        other => other.clone(),
    }
}

fn vdef_name(qualified: &str) -> &str {
    qualified.rsplit('.').next().unwrap_or(qualified)
}

/// The closest candidate within a small edit distance.
pub fn suggest<'b>(name: &str, candidates: impl Iterator<Item = &'b str>) -> Option<String> {
    let limit = (name.chars().count() / 3).max(1);
    let mut best: Option<(usize, &str)> = None;
    for c in candidates {
        if c == name {
            continue;
        }
        let d = edit_distance(name, c);
        if d <= limit && best.is_none_or(|(bd, bc)| d < bd || (d == bd && c < bc)) {
            best = Some((d, c));
        }
    }
    best.map(|(_, c)| c.to_string())
}

/// Optimal string alignment distance: edits plus adjacent transpositions.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().map(|c| c.to_ascii_lowercase()).collect();
    let b: Vec<char> = b.chars().map(|c| c.to_ascii_lowercase()).collect();
    let w = b.len() + 1;
    let mut d = vec![0usize; (a.len() + 1) * w];
    for i in 0..=a.len() {
        d[i * w] = i;
    }
    for (j, cell) in d.iter_mut().enumerate().take(w) {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut v = (d[(i - 1) * w + j] + 1)
                .min(d[i * w + j - 1] + 1)
                .min(d[(i - 1) * w + j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                v = v.min(d[(i - 2) * w + j - 2] + 1);
            }
            d[i * w + j] = v;
        }
    }
    d[a.len() * w + b.len()]
}
