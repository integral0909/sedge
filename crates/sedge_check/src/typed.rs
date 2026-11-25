//! What the checker produces: declarations plus side tables keyed by node
//! id, used by the compiler and the language server.

use crate::types::{AdtId, ParamId, Ty, TypeCon, TypeNames};
use sedge_syntax::ast::NodeId;
use sedge_syntax::Span;
use std::collections::HashMap;

pub type FnId = u32;
pub type LocalId = u32;

#[derive(Clone, Debug)]
pub struct ParamDef {
    pub name: String,
    pub bounds: Vec<Bound>,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bound {
    /// Supports `<`, `<=`, `>` and `>=` (and sorting).
    Ord,
    /// Supports arithmetic: Int or Float.
    Num,
    /// Can be a map key.
    Key,
}

impl Bound {
    pub fn from_name(name: &str) -> Option<Bound> {
        match name {
            "Ord" => Some(Bound::Ord),
            "Num" => Some(Bound::Num),
            "Key" => Some(Bound::Key),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Bound::Ord => "Ord",
            Bound::Num => "Num",
            Bound::Key => "Key",
        }
    }
}

#[derive(Clone, Debug)]
pub struct FieldDef {
    pub name: String,
    pub ty: Ty,
    pub span: Span,
    pub doc: Option<String>,
}

#[derive(Clone, Debug)]
pub struct VariantDef {
    pub name: String,
    pub fields: Vec<Ty>,
    pub span: Span,
    pub doc: Option<String>,
}

#[derive(Clone, Debug)]
pub enum AdtKind {
    Struct(Vec<FieldDef>),
    Enum(Vec<VariantDef>),
}

#[derive(Clone, Debug)]
pub struct AdtDef {
    pub name: String,
    pub params: Vec<ParamId>,
    pub kind: AdtKind,
    pub span: Span,
    pub doc: Option<String>,
}

impl AdtDef {
    pub fn variants(&self) -> &[VariantDef] {
        match &self.kind {
            AdtKind::Enum(v) => v,
            AdtKind::Struct(_) => &[],
        }
    }

    pub fn fields(&self) -> &[FieldDef] {
        match &self.kind {
            AdtKind::Struct(f) => f,
            AdtKind::Enum(_) => &[],
        }
    }

    pub fn is_enum(&self) -> bool {
        matches!(self.kind, AdtKind::Enum(_))
    }
}

/// Where a function's body lives in the syntax tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FnSource {
    Item {
        module: usize,
        item: usize,
    },
    Method {
        module: usize,
        item: usize,
        method: usize,
    },
}

#[derive(Clone, Debug)]
pub struct FnDef {
    /// `main`, or `Point.norm` for methods.
    pub name: String,
    pub short_name: String,
    pub owner: Option<TypeCon>,
    /// Type parameters, the impl's first.
    pub generics: Vec<ParamId>,
    pub has_self: bool,
    /// Parameter types, including `self`.
    pub params: Vec<Ty>,
    pub param_names: Vec<String>,
    pub ret: Ty,
    pub native: bool,
    pub source: FnSource,
    /// Locals for each parameter, including `self`.
    pub param_locals: Vec<LocalId>,
    pub span: Span,
    pub name_span: Span,
    pub doc: Option<String>,
    /// From the prelude rather than user code.
    pub builtin: bool,
}

#[derive(Clone, Debug)]
pub struct LocalDef {
    pub name: String,
    pub mutable: bool,
    pub ty: Ty,
    pub span: Span,
    /// Captured by a closure.
    pub captured: bool,
    /// Lives in a heap cell because a closure captures a `var`.
    pub boxed: bool,
}

#[derive(Clone, Debug)]
pub struct TestDef {
    pub name: String,
    pub module: usize,
    pub item: usize,
    pub span: Span,
}

/// What a name expression refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Res {
    Local(LocalId),
    Fn(FnId),
    /// A variant used as a value: a constant without fields, or a
    /// constructor function with them.
    Variant(AdtId, u32),
}

/// How a call or method call is carried out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallRes {
    /// A known function. For method calls the receiver is the first
    /// argument.
    Direct(FnId),
    /// A static method such as `Point.new(...)`: the receiver is a type name
    /// and is not passed.
    Static(FnId),
    /// Constructs an enum variant.
    Variant(AdtId, u32),
    /// Calls a closure value (the callee expression, or a struct field for
    /// method syntax).
    Indirect,
    /// `recv.field(args)` where `field` holds a closure.
    FieldClosure(u32),
}

/// What a pattern node does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PatRes {
    Bind(LocalId),
    Variant(AdtId, u32),
    Struct(AdtId),
}

#[derive(Clone, Debug)]
pub struct LambdaInfo {
    pub params: Vec<LocalId>,
    /// Captured locals of enclosing functions, in capture-slot order.
    pub captures: Vec<LocalId>,
}

/// A definition a name use can jump to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Def {
    Local(LocalId),
    Fn(FnId),
    Adt(AdtId),
    Variant(AdtId, u32),
    Field(AdtId, u32),
    Param(ParamId),
}

/// A name occurrence, for hover and go-to-definition.
#[derive(Clone, Debug)]
pub struct Use {
    pub span: Span,
    pub def: Def,
    pub ty: Ty,
}

#[derive(Default)]
pub struct Typed {
    pub adts: Vec<AdtDef>,
    pub params: Vec<ParamDef>,
    pub fns: Vec<FnDef>,
    pub locals: Vec<LocalDef>,
    pub tests: Vec<TestDef>,
    pub methods: HashMap<(TypeCon, String), FnId>,
    pub globals: HashMap<String, FnId>,

    pub expr_types: HashMap<NodeId, Ty>,
    pub names: HashMap<NodeId, Res>,
    pub calls: HashMap<NodeId, CallRes>,
    /// Field index for `Field` expressions and struct literals' fields are
    /// looked up by name in the struct definition.
    pub fields: HashMap<NodeId, u32>,
    pub patterns: HashMap<NodeId, PatRes>,
    pub lambdas: HashMap<NodeId, LambdaInfo>,
    /// Type of the pattern's value, for patterns that need it.
    pub pattern_types: HashMap<NodeId, Ty>,

    pub uses: Vec<Use>,
    /// Later alternatives of or-patterns, mapped to the first alternative's
    /// binding they share a local with.
    pub alias_patterns: Vec<(NodeId, NodeId)>,

    pub option_adt: Option<AdtId>,
    pub result_adt: Option<AdtId>,
}

impl TypeNames for Typed {
    fn adt_name(&self, id: AdtId) -> &str {
        &self.adts[id as usize].name
    }

    fn param_name(&self, id: ParamId) -> &str {
        &self.params[id as usize].name
    }
}

impl Typed {
    pub fn show(&self, ty: &Ty) -> String {
        crate::types::Display { ty, names: self }.to_string()
    }

    pub fn ty_of(&self, id: NodeId) -> &Ty {
        self.expr_types.get(&id).unwrap_or(&Ty::Error)
    }

    pub fn main_fn(&self) -> Option<FnId> {
        self.globals.get("main").copied()
    }

    /// The signature of a function as it would be written in source.
    pub fn signature(&self, id: FnId) -> String {
        let f = &self.fns[id as usize];
        let mut s = String::from("fn ");
        s.push_str(&f.name);
        let own: Vec<_> = f
            .generics
            .iter()
            .filter(|p| match f.owner {
                // Impl parameters show on the owner type, not the method.
                Some(_) => !f
                    .params
                    .first()
                    .is_some_and(|t| f.has_self && mentions_param(t, **p)),
                None => true,
            })
            .collect();
        if !own.is_empty() {
            s.push('[');
            for (i, p) in own.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                let def = &self.params[**p as usize];
                s.push_str(&def.name);
                if !def.bounds.is_empty() {
                    s.push_str(": ");
                    let b: Vec<_> = def.bounds.iter().map(|b| b.name()).collect();
                    s.push_str(&b.join(" + "));
                }
            }
            s.push(']');
        }
        s.push('(');
        let mut first = true;
        for (i, (name, ty)) in f.param_names.iter().zip(&f.params).enumerate() {
            if !first {
                s.push_str(", ");
            }
            first = false;
            if i == 0 && f.has_self {
                s.push_str("self");
            } else {
                s.push_str(name);
                s.push_str(": ");
                s.push_str(&self.show(ty));
            }
        }
        s.push(')');
        if f.ret != Ty::Unit {
            s.push_str(" -> ");
            s.push_str(&self.show(&f.ret));
        }
        s
    }
}

fn mentions_param(t: &Ty, p: ParamId) -> bool {
    let mut found = false;
    t.visit(&mut |x| found |= *x == Ty::Param(p));
    found
}
