//! The syntax tree.
//!
//! Expressions and patterns carry a [`NodeId`] that later passes use as a key
//! for side tables (types, name resolutions), so the tree itself stays
//! immutable after parsing.

use crate::source::Span;

pub type NodeId = u32;

#[derive(Clone, Debug, PartialEq)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Module {
    pub items: Vec<Item>,
}

#[derive(Clone, Debug)]
pub enum Item {
    Fn(FnDecl),
    Struct(StructDecl),
    Enum(EnumDecl),
    Impl(ImplDecl),
    Test(TestDecl),
}

impl Item {
    pub fn span(&self) -> Span {
        match self {
            Item::Fn(f) => f.span,
            Item::Struct(s) => s.span,
            Item::Enum(e) => e.span,
            Item::Impl(i) => i.span,
            Item::Test(t) => t.span,
        }
    }
}

#[derive(Clone, Debug)]
pub struct GenericParam {
    pub name: Ident,
    pub bounds: Vec<Ident>,
}

#[derive(Clone, Debug)]
pub struct WhereClause {
    pub param: Ident,
    pub bounds: Vec<Ident>,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub id: NodeId,
    pub name: Ident,
    pub ty: Option<TypeExpr>,
}

#[derive(Clone, Debug)]
pub struct FnDecl {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub where_clauses: Vec<WhereClause>,
    /// `Some(span)` when the first parameter is `self`.
    pub self_param: Option<Span>,
    pub params: Vec<Param>,
    pub ret: Option<TypeExpr>,
    /// `None` for `native fn`.
    pub body: Option<Block>,
    pub doc: Option<String>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Field {
    pub name: Ident,
    pub ty: TypeExpr,
    pub doc: Option<String>,
}

#[derive(Clone, Debug)]
pub struct StructDecl {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub fields: Vec<Field>,
    pub doc: Option<String>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Variant {
    pub name: Ident,
    pub fields: Vec<TypeExpr>,
    pub doc: Option<String>,
}

#[derive(Clone, Debug)]
pub struct EnumDecl {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub variants: Vec<Variant>,
    pub doc: Option<String>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct ImplDecl {
    /// The type being extended, such as `Array` in `impl Array[T]`.
    pub target: Ident,
    /// Type parameter names introduced by the target, such as `T`.
    pub params: Vec<Ident>,
    pub methods: Vec<FnDecl>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct TestDecl {
    pub name: String,
    pub body: Block,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct TypeExpr {
    pub kind: TypeExprKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum TypeExprKind {
    /// `Int`, `Option[T]`, `Map[String, Int]`
    Named(Ident, Vec<TypeExpr>),
    /// `[T]`
    Array(Box<TypeExpr>),
    /// `(A, B)`; `()` is the unit type.
    Tuple(Vec<TypeExpr>),
    /// `fn(A, B) -> C`
    Fn(Vec<TypeExpr>, Option<Box<TypeExpr>>),
}

#[derive(Clone, Debug)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssignOp {
    Set,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

#[derive(Clone, Debug)]
pub enum StmtKind {
    Let {
        pattern: Pattern,
        mutable: bool,
        ty: Option<TypeExpr>,
        init: Expr,
    },
    Assign {
        target: Expr,
        op: AssignOp,
        value: Expr,
    },
    While {
        cond: Expr,
        body: Block,
    },
    For {
        pattern: Pattern,
        iter: Expr,
        body: Block,
    },
    Return(Option<Expr>),
    Break,
    Continue,
    Expr(Expr),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

impl BinOp {
    pub fn symbol(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Rem => "%",
            BinOp::BitAnd => "&",
            BinOp::BitOr => "|",
            BinOp::BitXor => "^",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::And => "&&",
            BinOp::Or => "||",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub id: NodeId,
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum InterpPart {
    Lit(String),
    Expr(Expr),
}

#[derive(Clone, Debug)]
pub struct Arm {
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    pub body: Expr,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct FieldInit {
    pub name: Ident,
    /// The shorthand `Point { x, y }` is parsed as `Point { x: x, y: y }`.
    pub value: Expr,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Int(i64),
    Float(f64),
    Str(String),
    Interp(Vec<InterpPart>),
    Bool(bool),
    Unit,
    Name(String),
    SelfValue,
    Field {
        base: Box<Expr>,
        name: Ident,
    },
    TupleField {
        base: Box<Expr>,
        index: u32,
        index_span: Span,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    MethodCall {
        recv: Box<Expr>,
        name: Ident,
        args: Vec<Expr>,
    },
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Unary {
        op: UnOp,
        expr: Box<Expr>,
    },
    If {
        cond: Box<Expr>,
        then: Block,
        else_: Option<Box<Expr>>,
    },
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<Arm>,
    },
    Block(Block),
    Array(Vec<Expr>),
    Tuple(Vec<Expr>),
    StructLit {
        name: Ident,
        fields: Vec<FieldInit>,
    },
    Lambda {
        params: Vec<Param>,
        ret: Option<TypeExpr>,
        body: Box<Expr>,
    },
    Range {
        lo: Box<Expr>,
        hi: Box<Expr>,
        inclusive: bool,
    },
    Try(Box<Expr>),
    /// A placeholder left where parsing failed.
    Error,
}

#[derive(Clone, Debug)]
pub struct Pattern {
    pub id: NodeId,
    pub kind: PatternKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum PatternKind {
    Wild,
    /// A bare name: a binding, or a variant without fields if one exists.
    Name(String),
    Int(i64),
    Str(String),
    Bool(bool),
    /// `Some(x)`, `Shape.Circle(r)`
    Variant {
        enum_name: Option<Ident>,
        name: Ident,
        args: Vec<Pattern>,
    },
    Struct {
        name: Ident,
        fields: Vec<(Ident, Pattern)>,
        rest: bool,
    },
    Tuple(Vec<Pattern>),
    Or(Vec<Pattern>),
    Error,
}

impl Pattern {
    /// Calls `f` with each name the pattern binds and its pattern id.
    /// Bare names are reported too; callers that know a name is a variant
    /// skip it.
    pub fn walk(&self, f: &mut impl FnMut(&Pattern)) {
        f(self);
        match &self.kind {
            PatternKind::Variant { args, .. } => args.iter().for_each(|p| p.walk(f)),
            PatternKind::Struct { fields, .. } => {
                for (_, p) in fields {
                    p.walk(f);
                }
            }
            PatternKind::Tuple(ps) | PatternKind::Or(ps) => ps.iter().for_each(|p| p.walk(f)),
            _ => {}
        }
    }
}
