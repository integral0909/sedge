//! A recursive-descent parser with panic-mode error recovery.

use crate::ast::*;
use crate::diag::Diagnostic;
use crate::lexer::{lex, Lexed};
use crate::source::Span;
use crate::token::{StrPart, Token, TokenKind};

pub struct Parsed {
    pub module: Module,
    pub diagnostics: Vec<Diagnostic>,
}

/// Parses a whole file. `next_id` is the first node id to hand out and is
/// advanced past the last one used, so several files can share one id space.
pub fn parse_module(text: &str, base: u32, next_id: &mut NodeId) -> Parsed {
    let Lexed {
        tokens,
        docs,
        diagnostics,
    } = lex(text, base);
    let mut p = Parser {
        tokens,
        pos: 0,
        text,
        base,
        next_id,
        diagnostics,
        docs,
        doc_cursor: 0,
    };
    let module = p.module();
    Parsed {
        module,
        diagnostics: p.diagnostics,
    }
}

/// Parses a single expression (used for tests and the REPL).
pub fn parse_expr(text: &str, base: u32, next_id: &mut NodeId) -> (Option<Expr>, Vec<Diagnostic>) {
    let Lexed {
        tokens,
        diagnostics,
        docs,
    } = lex(text, base);
    let mut p = Parser {
        tokens,
        pos: 0,
        text,
        base,
        next_id,
        diagnostics,
        docs,
        doc_cursor: 0,
    };
    p.skip_semis();
    let e = p.expr(false).ok();
    p.skip_semis();
    if !p.at(&TokenKind::Eof) {
        let span = p.peek().span;
        p.diagnostics
            .push(Diagnostic::error(span, "unexpected input after expression"));
    }
    (e, p.diagnostics)
}

type PResult<T> = Result<T, ()>;

struct Parser<'a> {
    tokens: Vec<Token>,
    pos: usize,
    text: &'a str,
    base: u32,
    next_id: &'a mut NodeId,
    diagnostics: Vec<Diagnostic>,
    docs: Vec<(Span, String)>,
    doc_cursor: usize,
}

/// Binding power of binary operators; higher binds tighter.
fn binop(kind: &TokenKind) -> Option<(BinOp, u8)> {
    use TokenKind::*;
    Some(match kind {
        OrOr => (BinOp::Or, 1),
        AndAnd => (BinOp::And, 2),
        EqEq => (BinOp::Eq, 3),
        NotEq => (BinOp::Ne, 3),
        Lt => (BinOp::Lt, 3),
        Le => (BinOp::Le, 3),
        Gt => (BinOp::Gt, 3),
        Ge => (BinOp::Ge, 3),
        Pipe => (BinOp::BitOr, 4),
        Caret => (BinOp::BitXor, 5),
        Amp => (BinOp::BitAnd, 6),
        Shl => (BinOp::Shl, 7),
        Shr => (BinOp::Shr, 7),
        Plus => (BinOp::Add, 8),
        Minus => (BinOp::Sub, 8),
        Star => (BinOp::Mul, 9),
        Slash => (BinOp::Div, 9),
        Percent => (BinOp::Rem, 9),
        _ => return None,
    })
}

impl<'a> Parser<'a> {
    // ----- token helpers -------------------------------------------------

    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn peek_kind(&self) -> &TokenKind {
        &self.tokens[self.pos].kind
    }

    fn nth_kind(&self, n: usize) -> &TokenKind {
        let i = (self.pos + n).min(self.tokens.len() - 1);
        &self.tokens[i].kind
    }

    fn at(&self, kind: &TokenKind) -> bool {
        self.peek_kind() == kind
    }

    fn bump(&mut self) -> Token {
        let t = self.tokens[self.pos].clone();
        if self.pos < self.tokens.len() - 1 {
            self.pos += 1;
        }
        t
    }

    fn eat(&mut self, kind: &TokenKind) -> bool {
        if self.at(kind) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn prev_span(&self) -> Span {
        if self.pos == 0 {
            return self.peek().span;
        }
        // Skip implicit semicolons, which have empty spans.
        let mut i = self.pos - 1;
        while i > 0 && self.tokens[i].implicit {
            i -= 1;
        }
        self.tokens[i].span
    }

    fn error_here(&mut self, expected: &str) {
        let t = self.peek().clone();
        let found = if t.implicit {
            "end of line".to_string()
        } else {
            t.kind.to_string()
        };
        let span = if t.implicit || t.kind == TokenKind::Eof {
            let s = self.prev_span();
            Span::new(s.hi, s.hi)
        } else {
            t.span
        };
        self.diagnostics.push(
            Diagnostic::error(span, format!("expected {expected}, found {found}"))
                .with_label(format!("expected {expected}")),
        );
    }

    fn expect(&mut self, kind: &TokenKind) -> PResult<Token> {
        if self.at(kind) {
            Ok(self.bump())
        } else {
            self.error_here(&kind.to_string());
            Err(())
        }
    }

    fn skip_semis(&mut self) {
        while self.at(&TokenKind::Semi) {
            self.bump();
        }
    }

    fn ident(&mut self, what: &str) -> PResult<Ident> {
        match self.peek_kind().clone() {
            TokenKind::Ident(name) => {
                let t = self.bump();
                Ok(Ident { name, span: t.span })
            }
            _ => {
                self.error_here(what);
                Err(())
            }
        }
    }

    fn id(&mut self) -> NodeId {
        let id = *self.next_id;
        *self.next_id += 1;
        id
    }

    fn mk(&mut self, kind: ExprKind, span: Span) -> Expr {
        Expr {
            id: self.id(),
            kind,
            span,
        }
    }

    /// Takes the doc comment that ends right before `offset`, if any.
    fn doc_before(&mut self, offset: u32) -> Option<String> {
        let mut found = None;
        while self.doc_cursor < self.docs.len() && self.docs[self.doc_cursor].0.hi <= offset {
            let (span, text) = &self.docs[self.doc_cursor];
            let gap = &self.text[(span.hi - self.base) as usize..(offset - self.base) as usize];
            found = if gap.trim().is_empty() {
                Some(text.clone())
            } else {
                None
            };
            self.doc_cursor += 1;
        }
        found
    }

    // ----- items -----------------------------------------------------------

    fn module(&mut self) -> Module {
        let mut items = Vec::new();
        loop {
            self.skip_semis();
            if self.at(&TokenKind::Eof) {
                break;
            }
            match self.item() {
                Ok(item) => items.push(item),
                Err(()) => self.recover_item(),
            }
        }
        Module { items }
    }

    fn recover_item(&mut self) {
        // Skip to the next token that can start an item at the top level.
        let mut depth = 0i32;
        loop {
            match self.peek_kind() {
                TokenKind::Eof => return,
                TokenKind::LBrace => depth += 1,
                TokenKind::RBrace => {
                    depth -= 1;
                    if depth <= 0 {
                        self.bump();
                        if depth == 0 {
                            return;
                        }
                        depth = 0;
                        continue;
                    }
                }
                TokenKind::Fn
                | TokenKind::Struct
                | TokenKind::Enum
                | TokenKind::Impl
                | TokenKind::Native
                    if depth == 0 =>
                {
                    return
                }
                _ => {}
            }
            self.bump();
        }
    }

    fn item(&mut self) -> PResult<Item> {
        let start = self.peek().span;
        let doc = self.doc_before(start.lo);
        match self.peek_kind() {
            TokenKind::Fn | TokenKind::Native => Ok(Item::Fn(self.fn_decl(doc)?)),
            TokenKind::Struct => Ok(Item::Struct(self.struct_decl(doc)?)),
            TokenKind::Enum => Ok(Item::Enum(self.enum_decl(doc)?)),
            TokenKind::Impl => Ok(Item::Impl(self.impl_decl()?)),
            TokenKind::Ident(name)
                if name == "test" && matches!(self.nth_kind(1), TokenKind::Str(_)) =>
            {
                Ok(Item::Test(self.test_decl()?))
            }
            TokenKind::Let | TokenKind::Var => {
                let span = self.peek().span;
                self.diagnostics.push(
                    Diagnostic::error(span, "variables can only be declared inside functions")
                        .with_note("top-level code goes in `fn main() { ... }`"),
                );
                Err(())
            }
            _ => {
                self.error_here("an item (`fn`, `struct`, `enum`, `impl` or `test`)");
                Err(())
            }
        }
    }

    fn generics(&mut self) -> PResult<Vec<GenericParam>> {
        let mut out = Vec::new();
        if !self.eat(&TokenKind::LBracket) {
            return Ok(out);
        }
        while !self.at(&TokenKind::RBracket) {
            let name = self.ident("a type parameter name")?;
            let bounds = self.bounds()?;
            out.push(GenericParam { name, bounds });
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(&TokenKind::RBracket)?;
        Ok(out)
    }

    fn bounds(&mut self) -> PResult<Vec<Ident>> {
        let mut bounds = Vec::new();
        if self.eat(&TokenKind::Colon) {
            bounds.push(self.ident("a bound such as `Ord`")?);
            while self.eat(&TokenKind::Plus) {
                bounds.push(self.ident("a bound such as `Ord`")?);
            }
        }
        Ok(bounds)
    }

    fn fn_decl(&mut self, doc: Option<String>) -> PResult<FnDecl> {
        let start = self.peek().span;
        let native = self.eat(&TokenKind::Native);
        self.expect(&TokenKind::Fn)?;
        let name = self.ident("a function name")?;
        let generics = self.generics()?;
        self.expect(&TokenKind::LParen)?;
        let mut self_param = None;
        let mut params = Vec::new();
        if self.at(&TokenKind::SelfKw) {
            self_param = Some(self.bump().span);
            if !self.eat(&TokenKind::Comma) && !self.at(&TokenKind::RParen) {
                self.error_here("`,` or `)`");
                return Err(());
            }
        }
        while !self.at(&TokenKind::RParen) {
            let pname = self.ident("a parameter name")?;
            self.expect(&TokenKind::Colon)?;
            let ty = self.type_expr()?;
            let id = self.id();
            params.push(Param {
                id,
                name: pname,
                ty: Some(ty),
            });
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(&TokenKind::RParen)?;
        let ret = if self.eat(&TokenKind::Arrow) {
            Some(self.type_expr()?)
        } else {
            None
        };
        let mut where_clauses = Vec::new();
        if self.eat(&TokenKind::Where) {
            loop {
                let param = self.ident("a type parameter name")?;
                let bounds = self.bounds()?;
                where_clauses.push(WhereClause { param, bounds });
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
        }
        let body = if native { None } else { Some(self.block()?) };
        Ok(FnDecl {
            name,
            generics,
            where_clauses,
            self_param,
            params,
            ret,
            body,
            doc,
            span: start.to(self.prev_span()),
        })
    }

    fn struct_decl(&mut self, doc: Option<String>) -> PResult<StructDecl> {
        let start = self.bump().span;
        let name = self.ident("a struct name")?;
        let generics = self.generics()?;
        self.expect(&TokenKind::LBrace)?;
        let mut fields = Vec::new();
        loop {
            self.skip_semis();
            if self.at(&TokenKind::RBrace) {
                break;
            }
            let fdoc = self.doc_before(self.peek().span.lo);
            let fname = self.ident("a field name")?;
            self.expect(&TokenKind::Colon)?;
            let ty = self.type_expr()?;
            fields.push(Field {
                name: fname,
                ty,
                doc: fdoc,
            });
            if !self.eat(&TokenKind::Comma) && !self.at(&TokenKind::Semi) {
                break;
            }
        }
        self.skip_semis();
        self.expect(&TokenKind::RBrace)?;
        Ok(StructDecl {
            name,
            generics,
            fields,
            doc,
            span: start.to(self.prev_span()),
        })
    }

    fn enum_decl(&mut self, doc: Option<String>) -> PResult<EnumDecl> {
        let start = self.bump().span;
        let name = self.ident("an enum name")?;
        let generics = self.generics()?;
        self.expect(&TokenKind::LBrace)?;
        let mut variants = Vec::new();
        loop {
            self.skip_semis();
            if self.at(&TokenKind::RBrace) {
                break;
            }
            let vdoc = self.doc_before(self.peek().span.lo);
            let vname = self.ident("a variant name")?;
            let mut vfields = Vec::new();
            if self.eat(&TokenKind::LParen) {
                while !self.at(&TokenKind::RParen) {
                    vfields.push(self.type_expr()?);
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(&TokenKind::RParen)?;
            }
            variants.push(Variant {
                name: vname,
                fields: vfields,
                doc: vdoc,
            });
            if !self.eat(&TokenKind::Comma) && !self.at(&TokenKind::Semi) {
                break;
            }
        }
        self.skip_semis();
        self.expect(&TokenKind::RBrace)?;
        Ok(EnumDecl {
            name,
            generics,
            variants,
            doc,
            span: start.to(self.prev_span()),
        })
    }

    fn impl_decl(&mut self) -> PResult<ImplDecl> {
        let start = self.bump().span;
        let target = self.ident("a type name")?;
        let mut params = Vec::new();
        if self.eat(&TokenKind::LBracket) {
            while !self.at(&TokenKind::RBracket) {
                params.push(self.ident("a type parameter name")?);
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
            self.expect(&TokenKind::RBracket)?;
        }
        self.expect(&TokenKind::LBrace)?;
        let mut methods = Vec::new();
        loop {
            self.skip_semis();
            if self.at(&TokenKind::RBrace) || self.at(&TokenKind::Eof) {
                break;
            }
            let doc = self.doc_before(self.peek().span.lo);
            if !matches!(self.peek_kind(), TokenKind::Fn | TokenKind::Native) {
                self.error_here("a method (`fn`)");
                return Err(());
            }
            methods.push(self.fn_decl(doc)?);
        }
        self.expect(&TokenKind::RBrace)?;
        Ok(ImplDecl {
            target,
            params,
            methods,
            span: start.to(self.prev_span()),
        })
    }

    fn test_decl(&mut self) -> PResult<TestDecl> {
        let start = self.bump().span;
        let name = match self.peek_kind().clone() {
            TokenKind::Str(parts) => {
                self.bump();
                match parts.as_slice() {
                    [StrPart::Lit(s)] => s.clone(),
                    _ => {
                        let span = self.prev_span();
                        self.diagnostics.push(Diagnostic::error(
                            span,
                            "test names cannot contain `${...}`",
                        ));
                        String::new()
                    }
                }
            }
            _ => {
                self.error_here("a test name string");
                return Err(());
            }
        };
        let body = self.block()?;
        Ok(TestDecl {
            name,
            body,
            span: start.to(self.prev_span()),
        })
    }

    // ----- types -------------------------------------------------------------

    fn type_expr(&mut self) -> PResult<TypeExpr> {
        let start = self.peek().span;
        let kind = match self.peek_kind().clone() {
            TokenKind::Ident(name) => {
                let t = self.bump();
                let mut args = Vec::new();
                if self.eat(&TokenKind::LBracket) {
                    while !self.at(&TokenKind::RBracket) {
                        args.push(self.type_expr()?);
                        if !self.eat(&TokenKind::Comma) {
                            break;
                        }
                    }
                    self.expect(&TokenKind::RBracket)?;
                }
                TypeExprKind::Named(Ident { name, span: t.span }, args)
            }
            TokenKind::LBracket => {
                self.bump();
                let elem = self.type_expr()?;
                self.expect(&TokenKind::RBracket)?;
                TypeExprKind::Array(Box::new(elem))
            }
            TokenKind::LParen => {
                self.bump();
                let mut elems = Vec::new();
                let mut trailing_comma = false;
                while !self.at(&TokenKind::RParen) {
                    elems.push(self.type_expr()?);
                    trailing_comma = false;
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                    trailing_comma = true;
                }
                self.expect(&TokenKind::RParen)?;
                if elems.len() == 1 && !trailing_comma {
                    return Ok(elems.pop().unwrap());
                }
                TypeExprKind::Tuple(elems)
            }
            TokenKind::Fn => {
                self.bump();
                self.expect(&TokenKind::LParen)?;
                let mut params = Vec::new();
                while !self.at(&TokenKind::RParen) {
                    params.push(self.type_expr()?);
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(&TokenKind::RParen)?;
                let ret = if self.eat(&TokenKind::Arrow) {
                    Some(Box::new(self.type_expr()?))
                } else {
                    None
                };
                TypeExprKind::Fn(params, ret)
            }
            _ => {
                self.error_here("a type");
                return Err(());
            }
        };
        Ok(TypeExpr {
            kind,
            span: start.to(self.prev_span()),
        })
    }

    // ----- statements ------------------------------------------------------

    fn block(&mut self) -> PResult<Block> {
        let start = self.expect(&TokenKind::LBrace)?.span;
        let mut stmts = Vec::new();
        loop {
            self.skip_semis();
            if self.at(&TokenKind::RBrace) || self.at(&TokenKind::Eof) {
                break;
            }
            match self.stmt() {
                Ok(s) => {
                    let block_like =
                        matches!(&s.kind, StmtKind::While { .. } | StmtKind::For { .. })
                            || matches!(&s.kind, StmtKind::Expr(e) if is_block_like(e));
                    stmts.push(s);
                    if !block_like && !self.at(&TokenKind::Semi) && !self.at(&TokenKind::RBrace) {
                        self.error_here("`;` or a line break");
                        self.recover_stmt();
                    }
                }
                Err(()) => self.recover_stmt(),
            }
        }
        self.expect(&TokenKind::RBrace)?;
        Ok(Block {
            stmts,
            span: start.to(self.prev_span()),
        })
    }

    fn recover_stmt(&mut self) {
        let mut depth = 0i32;
        loop {
            match self.peek_kind() {
                TokenKind::Eof => return,
                TokenKind::Semi if depth == 0 => {
                    self.bump();
                    return;
                }
                TokenKind::LBrace | TokenKind::LParen | TokenKind::LBracket => depth += 1,
                TokenKind::RBrace if depth == 0 => return,
                TokenKind::RBrace | TokenKind::RParen | TokenKind::RBracket => {
                    depth -= 1;
                    if depth < 0 {
                        return;
                    }
                }
                _ => {}
            }
            self.bump();
        }
    }

    fn stmt(&mut self) -> PResult<Stmt> {
        let start = self.peek().span;
        let kind = match self.peek_kind() {
            TokenKind::Let | TokenKind::Var => {
                let mutable = self.bump().kind == TokenKind::Var;
                let pattern = self.pattern()?;
                let ty = if self.eat(&TokenKind::Colon) {
                    Some(self.type_expr()?)
                } else {
                    None
                };
                self.expect(&TokenKind::Eq)?;
                let init = self.expr(false)?;
                StmtKind::Let {
                    pattern,
                    mutable,
                    ty,
                    init,
                }
            }
            TokenKind::While => {
                self.bump();
                let cond = self.expr(true)?;
                let body = self.block()?;
                StmtKind::While { cond, body }
            }
            TokenKind::For => {
                self.bump();
                let pattern = self.pattern()?;
                self.expect(&TokenKind::In)?;
                let iter = self.expr(true)?;
                let body = self.block()?;
                StmtKind::For {
                    pattern,
                    iter,
                    body,
                }
            }
            TokenKind::Return => {
                self.bump();
                if matches!(
                    self.peek_kind(),
                    TokenKind::Semi | TokenKind::RBrace | TokenKind::Comma | TokenKind::RParen
                ) {
                    StmtKind::Return(None)
                } else {
                    StmtKind::Return(Some(self.expr(false)?))
                }
            }
            TokenKind::Break => {
                self.bump();
                StmtKind::Break
            }
            TokenKind::Continue => {
                self.bump();
                StmtKind::Continue
            }
            _ => {
                let e = self.expr(false)?;
                let op = match self.peek_kind() {
                    TokenKind::Eq => Some(AssignOp::Set),
                    TokenKind::PlusEq => Some(AssignOp::Add),
                    TokenKind::MinusEq => Some(AssignOp::Sub),
                    TokenKind::StarEq => Some(AssignOp::Mul),
                    TokenKind::SlashEq => Some(AssignOp::Div),
                    TokenKind::PercentEq => Some(AssignOp::Rem),
                    _ => None,
                };
                if let Some(op) = op {
                    self.bump();
                    let value = self.expr(false)?;
                    StmtKind::Assign {
                        target: e,
                        op,
                        value,
                    }
                } else {
                    StmtKind::Expr(e)
                }
            }
        };
        Ok(Stmt {
            kind,
            span: start.to(self.prev_span()),
        })
    }

    // ----- expressions -----------------------------------------------------

    /// `no_struct` forbids struct literals at the top level, as in the
    /// condition of `if`, where `{` starts the body.
    fn expr(&mut self, no_struct: bool) -> PResult<Expr> {
        let lhs = self.binary(0, no_struct)?;
        let inclusive = match self.peek_kind() {
            TokenKind::DotDot => false,
            TokenKind::DotDotEq => true,
            _ => return Ok(lhs),
        };
        self.bump();
        let rhs = self.binary(0, no_struct)?;
        let span = lhs.span.to(rhs.span);
        Ok(self.mk(
            ExprKind::Range {
                lo: Box::new(lhs),
                hi: Box::new(rhs),
                inclusive,
            },
            span,
        ))
    }

    fn binary(&mut self, min: u8, no_struct: bool) -> PResult<Expr> {
        let mut lhs = self.unary(no_struct)?;
        while let Some((op, prec)) = binop(self.peek_kind()) {
            if prec <= min {
                break;
            }
            let op_span = self.bump().span;
            let rhs = self.binary(prec, no_struct)?;
            if prec == 3 {
                if let ExprKind::Binary { op: inner, .. } = &lhs.kind {
                    if binop_prec(*inner) == 3 {
                        self.diagnostics.push(
                            Diagnostic::error(op_span, "comparison operators cannot be chained")
                                .with_note("use `&&` to combine comparisons: `a < b && b < c`"),
                        );
                    }
                }
            }
            let span = lhs.span.to(rhs.span);
            lhs = self.mk(
                ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                span,
            );
        }
        Ok(lhs)
    }

    fn unary(&mut self, no_struct: bool) -> PResult<Expr> {
        let start = self.peek().span;
        let op = match self.peek_kind() {
            TokenKind::Minus => UnOp::Neg,
            TokenKind::Bang => UnOp::Not,
            _ => return self.postfix(no_struct),
        };
        self.bump();
        let expr = self.unary(no_struct)?;
        let span = start.to(expr.span);
        // Fold negative literals so `-9223372036854775808` works.
        if op == UnOp::Neg {
            match expr.kind {
                ExprKind::Int(v) => return Ok(self.mk(ExprKind::Int(v.wrapping_neg()), span)),
                ExprKind::Float(v) => return Ok(self.mk(ExprKind::Float(-v), span)),
                _ => {}
            }
        }
        Ok(self.mk(
            ExprKind::Unary {
                op,
                expr: Box::new(expr),
            },
            span,
        ))
    }

    fn postfix(&mut self, no_struct: bool) -> PResult<Expr> {
        let mut e = self.primary(no_struct)?;
        loop {
            match self.peek_kind() {
                TokenKind::LParen => {
                    self.bump();
                    let args = self.call_args()?;
                    let span = e.span.to(self.prev_span());
                    e = match e.kind {
                        ExprKind::Field { base, name } => self.mk(
                            ExprKind::MethodCall {
                                recv: base,
                                name,
                                args,
                            },
                            span,
                        ),
                        _ => self.mk(
                            ExprKind::Call {
                                callee: Box::new(e),
                                args,
                            },
                            span,
                        ),
                    };
                }
                TokenKind::LBracket => {
                    self.bump();
                    let index = self.expr(false)?;
                    self.expect(&TokenKind::RBracket)?;
                    let span = e.span.to(self.prev_span());
                    e = self.mk(
                        ExprKind::Index {
                            base: Box::new(e),
                            index: Box::new(index),
                        },
                        span,
                    );
                }
                TokenKind::Dot => {
                    self.bump();
                    match self.peek_kind().clone() {
                        TokenKind::Int(i) => {
                            let t = self.bump();
                            let span = e.span.to(t.span);
                            e = self.mk(
                                ExprKind::TupleField {
                                    base: Box::new(e),
                                    index: i as u32,
                                    index_span: t.span,
                                },
                                span,
                            );
                        }
                        _ => {
                            let name = self.ident("a field or method name")?;
                            let span = e.span.to(name.span);
                            e = self.mk(
                                ExprKind::Field {
                                    base: Box::new(e),
                                    name,
                                },
                                span,
                            );
                        }
                    }
                }
                TokenKind::Question => {
                    let t = self.bump();
                    let span = e.span.to(t.span);
                    e = self.mk(ExprKind::Try(Box::new(e)), span);
                }
                _ => return Ok(e),
            }
        }
    }

    /// An expression, or an assignment wrapped in a block, for bodies like
    /// `Some(n) => total += n` and `fn(x) => count = x`.
    fn expr_or_assign(&mut self) -> PResult<Expr> {
        let e = self.expr(false)?;
        let op = match self.peek_kind() {
            TokenKind::Eq => AssignOp::Set,
            TokenKind::PlusEq => AssignOp::Add,
            TokenKind::MinusEq => AssignOp::Sub,
            TokenKind::StarEq => AssignOp::Mul,
            TokenKind::SlashEq => AssignOp::Div,
            TokenKind::PercentEq => AssignOp::Rem,
            _ => return Ok(e),
        };
        self.bump();
        let value = self.expr(false)?;
        let span = e.span.to(value.span);
        let stmt = Stmt {
            kind: StmtKind::Assign {
                target: e,
                op,
                value,
            },
            span,
        };
        Ok(self.mk(
            ExprKind::Block(Block {
                stmts: vec![stmt],
                span,
            }),
            span,
        ))
    }

    fn call_args(&mut self) -> PResult<Vec<Expr>> {
        let mut args = Vec::new();
        while !self.at(&TokenKind::RParen) {
            args.push(self.expr(false)?);
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(&TokenKind::RParen)?;
        Ok(args)
    }

    fn primary(&mut self, no_struct: bool) -> PResult<Expr> {
        let t = self.peek().clone();
        let span = t.span;
        match t.kind {
            TokenKind::Int(v) => {
                self.bump();
                Ok(self.mk(ExprKind::Int(v), span))
            }
            TokenKind::Float(v) => {
                self.bump();
                Ok(self.mk(ExprKind::Float(v), span))
            }
            TokenKind::True | TokenKind::False => {
                self.bump();
                Ok(self.mk(ExprKind::Bool(t.kind == TokenKind::True), span))
            }
            TokenKind::Str(parts) => {
                self.bump();
                self.string(parts, span)
            }
            TokenKind::SelfKw => {
                self.bump();
                Ok(self.mk(ExprKind::SelfValue, span))
            }
            TokenKind::Ident(name) => {
                self.bump();
                let starts_upper = name.chars().next().is_some_and(|c| c.is_uppercase());
                if !no_struct
                    && starts_upper
                    && self.at(&TokenKind::LBrace)
                    && self.looks_like_struct_lit()
                {
                    return self.struct_lit(Ident { name, span });
                }
                Ok(self.mk(ExprKind::Name(name), span))
            }
            TokenKind::LParen => {
                self.bump();
                if self.eat(&TokenKind::RParen) {
                    return Ok(self.mk(ExprKind::Unit, span.to(self.prev_span())));
                }
                let first = self.expr(false)?;
                if self.eat(&TokenKind::RParen) {
                    // Keep the parentheses in the span for diagnostics.
                    let mut e = first;
                    e.span = span.to(self.prev_span());
                    return Ok(e);
                }
                let mut elems = vec![first];
                while self.eat(&TokenKind::Comma) {
                    if self.at(&TokenKind::RParen) {
                        break;
                    }
                    elems.push(self.expr(false)?);
                }
                self.expect(&TokenKind::RParen)?;
                Ok(self.mk(ExprKind::Tuple(elems), span.to(self.prev_span())))
            }
            TokenKind::LBracket => {
                self.bump();
                let mut elems = Vec::new();
                while !self.at(&TokenKind::RBracket) {
                    elems.push(self.expr(false)?);
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(&TokenKind::RBracket)?;
                Ok(self.mk(ExprKind::Array(elems), span.to(self.prev_span())))
            }
            TokenKind::LBrace => {
                let b = self.block()?;
                let span = b.span;
                Ok(self.mk(ExprKind::Block(b), span))
            }
            TokenKind::If => self.if_expr(),
            TokenKind::Match => self.match_expr(),
            // `return`, `break` and `continue` in expression position, as in
            // `None => break`, become a block holding the statement.
            TokenKind::Return | TokenKind::Break | TokenKind::Continue => {
                let stmt = self.stmt()?;
                let span = stmt.span;
                Ok(self.mk(
                    ExprKind::Block(Block {
                        stmts: vec![stmt],
                        span,
                    }),
                    span,
                ))
            }
            TokenKind::Fn => self.lambda(),
            _ => {
                self.error_here("an expression");
                Err(())
            }
        }
    }

    /// After `Name {`: a struct literal looks like `}` or `ident :` or
    /// `ident ,` or `ident }`.
    fn looks_like_struct_lit(&self) -> bool {
        matches!(
            (self.nth_kind(1), self.nth_kind(2)),
            (TokenKind::RBrace, _)
                | (
                    TokenKind::Ident(_),
                    TokenKind::Colon | TokenKind::Comma | TokenKind::RBrace
                )
                | (TokenKind::Semi, _)
        )
    }

    fn struct_lit(&mut self, name: Ident) -> PResult<Expr> {
        self.expect(&TokenKind::LBrace)?;
        let mut fields = Vec::new();
        loop {
            self.skip_semis();
            if self.at(&TokenKind::RBrace) {
                break;
            }
            let fname = self.ident("a field name")?;
            let value = if self.eat(&TokenKind::Colon) {
                self.expr(false)?
            } else {
                self.mk(ExprKind::Name(fname.name.clone()), fname.span)
            };
            fields.push(FieldInit { name: fname, value });
            self.skip_semis();
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.skip_semis();
        self.expect(&TokenKind::RBrace)?;
        let span = name.span.to(self.prev_span());
        Ok(self.mk(ExprKind::StructLit { name, fields }, span))
    }

    fn string(&mut self, parts: Vec<StrPart>, span: Span) -> PResult<Expr> {
        if let [StrPart::Lit(s)] = parts.as_slice() {
            return Ok(self.mk(ExprKind::Str(s.clone()), span));
        }
        let mut out = Vec::new();
        for part in parts {
            match part {
                StrPart::Lit(s) => out.push(InterpPart::Lit(s)),
                StrPart::Hole(hole) => {
                    let local =
                        &self.text[(hole.lo - self.base) as usize..(hole.hi - self.base) as usize];
                    if local.trim().is_empty() {
                        self.diagnostics
                            .push(Diagnostic::error(hole, "empty `${}` in string"));
                        continue;
                    }
                    let (e, diags) = parse_expr(local, hole.lo, self.next_id);
                    self.diagnostics.extend(diags);
                    match e {
                        Some(e) => out.push(InterpPart::Expr(e)),
                        None => return Err(()),
                    }
                }
            }
        }
        Ok(self.mk(ExprKind::Interp(out), span))
    }

    fn if_expr(&mut self) -> PResult<Expr> {
        let start = self.bump().span;
        let cond = self.expr(true)?;
        let then = self.block()?;
        // Allow `else` on the line after the closing brace.
        if self.at(&TokenKind::Semi) && self.peek().implicit && self.nth_kind(1) == &TokenKind::Else
        {
            self.bump();
        }
        let else_ = if self.eat(&TokenKind::Else) {
            if self.at(&TokenKind::If) {
                Some(Box::new(self.if_expr()?))
            } else {
                let b = self.block()?;
                let span = b.span;
                Some(Box::new(self.mk(ExprKind::Block(b), span)))
            }
        } else {
            None
        };
        let span = start.to(self.prev_span());
        Ok(self.mk(
            ExprKind::If {
                cond: Box::new(cond),
                then,
                else_,
            },
            span,
        ))
    }

    fn match_expr(&mut self) -> PResult<Expr> {
        let start = self.bump().span;
        let scrutinee = self.expr(true)?;
        self.expect(&TokenKind::LBrace)?;
        let mut arms = Vec::new();
        loop {
            self.skip_semis();
            if self.at(&TokenKind::RBrace) || self.at(&TokenKind::Eof) {
                break;
            }
            let pattern = self.pattern()?;
            let guard = if self.eat(&TokenKind::If) {
                Some(self.expr(false)?)
            } else {
                None
            };
            self.expect(&TokenKind::FatArrow)?;
            let body = self.expr_or_assign()?;
            let span = pattern.span.to(body.span);
            arms.push(Arm {
                pattern,
                guard,
                body,
                span,
            });
            if !self.eat(&TokenKind::Comma)
                && !self.at(&TokenKind::Semi)
                && !self.at(&TokenKind::RBrace)
            {
                self.error_here("`,` or a line break between match arms");
                return Err(());
            }
        }
        self.expect(&TokenKind::RBrace)?;
        let span = start.to(self.prev_span());
        Ok(self.mk(
            ExprKind::Match {
                scrutinee: Box::new(scrutinee),
                arms,
            },
            span,
        ))
    }

    fn lambda(&mut self) -> PResult<Expr> {
        let start = self.bump().span;
        self.expect(&TokenKind::LParen)?;
        let mut params = Vec::new();
        while !self.at(&TokenKind::RParen) {
            let name = self.ident("a parameter name")?;
            let ty = if self.eat(&TokenKind::Colon) {
                Some(self.type_expr()?)
            } else {
                None
            };
            let id = self.id();
            params.push(Param { id, name, ty });
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(&TokenKind::RParen)?;
        let ret = if self.eat(&TokenKind::Arrow) {
            Some(self.type_expr()?)
        } else {
            None
        };
        let body = if self.eat(&TokenKind::FatArrow) {
            self.expr_or_assign()?
        } else if self.at(&TokenKind::LBrace) {
            let b = self.block()?;
            let span = b.span;
            self.mk(ExprKind::Block(b), span)
        } else {
            self.error_here("`=>` or `{`");
            return Err(());
        };
        let span = start.to(body.span);
        Ok(self.mk(
            ExprKind::Lambda {
                params,
                ret,
                body: Box::new(body),
            },
            span,
        ))
    }

    // ----- patterns --------------------------------------------------------

    fn pattern(&mut self) -> PResult<Pattern> {
        let first = self.pattern_alt()?;
        if !self.at(&TokenKind::Pipe) {
            return Ok(first);
        }
        let mut alts = vec![first];
        while self.eat(&TokenKind::Pipe) {
            alts.push(self.pattern_alt()?);
        }
        let span = alts[0].span.to(alts.last().unwrap().span);
        Ok(Pattern {
            id: self.id(),
            kind: PatternKind::Or(alts),
            span,
        })
    }

    fn pat(&mut self, kind: PatternKind, span: Span) -> Pattern {
        Pattern {
            id: self.id(),
            kind,
            span,
        }
    }

    fn pattern_alt(&mut self) -> PResult<Pattern> {
        let t = self.peek().clone();
        let span = t.span;
        match t.kind {
            TokenKind::Underscore => {
                self.bump();
                Ok(self.pat(PatternKind::Wild, span))
            }
            TokenKind::Int(v) => {
                self.bump();
                Ok(self.pat(PatternKind::Int(v), span))
            }
            TokenKind::Minus => {
                self.bump();
                match self.peek_kind().clone() {
                    TokenKind::Int(v) => {
                        let t2 = self.bump();
                        Ok(self.pat(PatternKind::Int(v.wrapping_neg()), span.to(t2.span)))
                    }
                    _ => {
                        self.error_here("an integer");
                        Err(())
                    }
                }
            }
            TokenKind::True | TokenKind::False => {
                self.bump();
                Ok(self.pat(PatternKind::Bool(t.kind == TokenKind::True), span))
            }
            TokenKind::Str(parts) => {
                self.bump();
                match parts.as_slice() {
                    [StrPart::Lit(s)] => Ok(self.pat(PatternKind::Str(s.clone()), span)),
                    _ => {
                        self.diagnostics.push(Diagnostic::error(
                            span,
                            "string patterns cannot contain `${...}`",
                        ));
                        Ok(self.pat(PatternKind::Error, span))
                    }
                }
            }
            TokenKind::LParen => {
                self.bump();
                let mut elems = Vec::new();
                let mut trailing = false;
                while !self.at(&TokenKind::RParen) {
                    elems.push(self.pattern()?);
                    trailing = false;
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                    trailing = true;
                }
                self.expect(&TokenKind::RParen)?;
                let span = span.to(self.prev_span());
                if elems.len() == 1 && !trailing {
                    let mut p = elems.pop().unwrap();
                    p.span = span;
                    return Ok(p);
                }
                Ok(self.pat(PatternKind::Tuple(elems), span))
            }
            TokenKind::Ident(name) => {
                self.bump();
                let mut ident = Ident { name, span };
                let mut enum_name = None;
                if self.at(&TokenKind::Dot) {
                    self.bump();
                    let variant = self.ident("a variant name")?;
                    enum_name = Some(ident);
                    ident = variant;
                }
                if self.eat(&TokenKind::LParen) {
                    let mut args = Vec::new();
                    while !self.at(&TokenKind::RParen) {
                        args.push(self.pattern()?);
                        if !self.eat(&TokenKind::Comma) {
                            break;
                        }
                    }
                    self.expect(&TokenKind::RParen)?;
                    let full = span.to(self.prev_span());
                    return Ok(self.pat(
                        PatternKind::Variant {
                            enum_name,
                            name: ident,
                            args,
                        },
                        full,
                    ));
                }
                if enum_name.is_none()
                    && self.at(&TokenKind::LBrace)
                    && self.looks_like_struct_pat()
                {
                    return self.struct_pattern(ident);
                }
                if enum_name.is_some() {
                    let full = span.to(ident.span);
                    return Ok(self.pat(
                        PatternKind::Variant {
                            enum_name,
                            name: ident,
                            args: Vec::new(),
                        },
                        full,
                    ));
                }
                Ok(self.pat(PatternKind::Name(ident.name), span))
            }
            _ => {
                self.error_here("a pattern");
                Err(())
            }
        }
    }

    fn looks_like_struct_pat(&self) -> bool {
        matches!(
            (self.nth_kind(1), self.nth_kind(2)),
            (TokenKind::RBrace, _)
                | (TokenKind::DotDot, _)
                | (
                    TokenKind::Ident(_),
                    TokenKind::Colon | TokenKind::Comma | TokenKind::RBrace
                )
        )
    }

    fn struct_pattern(&mut self, name: Ident) -> PResult<Pattern> {
        self.expect(&TokenKind::LBrace)?;
        let mut fields = Vec::new();
        let mut rest = false;
        loop {
            self.skip_semis();
            if self.at(&TokenKind::RBrace) {
                break;
            }
            if self.eat(&TokenKind::DotDot) {
                rest = true;
                self.skip_semis();
                break;
            }
            let fname = self.ident("a field name")?;
            let pat = if self.eat(&TokenKind::Colon) {
                self.pattern()?
            } else {
                let span = fname.span;
                self.pat(PatternKind::Name(fname.name.clone()), span)
            };
            fields.push((fname, pat));
            self.skip_semis();
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.skip_semis();
        self.expect(&TokenKind::RBrace)?;
        let span = name.span.to(self.prev_span());
        Ok(self.pat(PatternKind::Struct { name, fields, rest }, span))
    }
}

fn binop_prec(op: BinOp) -> u8 {
    match op {
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 3,
        _ => 0,
    }
}

/// Expressions that end in a block and so need no `;` after them.
pub fn is_block_like(e: &Expr) -> bool {
    matches!(
        e.kind,
        ExprKind::If { .. } | ExprKind::Match { .. } | ExprKind::Block(_)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(src: &str) -> Module {
        let mut id = 0;
        let p = parse_module(src, 0, &mut id);
        assert!(
            p.diagnostics.is_empty(),
            "unexpected diagnostics: {:#?}",
            p.diagnostics
        );
        p.module
    }

    fn errors(src: &str) -> Vec<String> {
        let mut id = 0;
        parse_module(src, 0, &mut id)
            .diagnostics
            .into_iter()
            .map(|d| d.message)
            .collect()
    }

    fn body(m: &Module) -> &Block {
        match &m.items[0] {
            Item::Fn(f) => f.body.as_ref().unwrap(),
            _ => panic!(),
        }
    }

    #[test]
    fn items() {
        let m = parse_ok(
            "/// A point.\nstruct Point { x: Float, y: Float }\n\
             enum Shape[T] { Circle(Float), Square(T,), Empty }\n\
             impl Point { fn norm(self) -> Float { self.x } }\n\
             native fn print[T](x: T)\n\
             fn max[T: Ord](a: T, b: T) -> T where T: Ord { if a > b { a } else { b } }\n\
             test \"adds\" { assert(1 + 1 == 2) }\n",
        );
        assert_eq!(m.items.len(), 6);
        match &m.items[0] {
            Item::Struct(s) => {
                assert_eq!(s.doc.as_deref(), Some("A point."));
                assert_eq!(s.fields.len(), 2);
            }
            _ => panic!(),
        }
        match &m.items[1] {
            Item::Enum(e) => {
                assert_eq!(e.variants.len(), 3);
                assert_eq!(e.variants[1].fields.len(), 1);
            }
            _ => panic!(),
        }
        match &m.items[3] {
            Item::Fn(f) => assert!(f.body.is_none()),
            _ => panic!(),
        }
    }

    #[test]
    fn precedence() {
        let mut id = 0;
        let (e, d) = parse_expr("1 + 2 * 3 == 7 && !false || x < 2", 0, &mut id);
        assert!(d.is_empty());
        match e.unwrap().kind {
            ExprKind::Binary {
                op: BinOp::Or, lhs, ..
            } => match lhs.kind {
                ExprKind::Binary {
                    op: BinOp::And,
                    lhs,
                    ..
                } => match lhs.kind {
                    ExprKind::Binary {
                        op: BinOp::Eq, lhs, ..
                    } => {
                        assert!(matches!(lhs.kind, ExprKind::Binary { op: BinOp::Add, .. }))
                    }
                    other => panic!("{other:?}"),
                },
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn statements_and_newlines() {
        let m = parse_ok(
            "fn main() {\n  var x = 1\n  x += 2\n  if x > 2 {\n    print(x)\n  }\n  else {\n    print(0)\n  }\n  let ys = xs\n    .map(fn(v) => v * 2)\n    .filter(fn(v) => v > 2)\n  for (i, y) in ys.enumerate() { print(y) }\n  while x < 10 { x = x + 1 }\n  return\n}\n",
        );
        let b = body(&m);
        assert_eq!(b.stmts.len(), 7);
        assert!(matches!(
            b.stmts[1].kind,
            StmtKind::Assign {
                op: AssignOp::Add,
                ..
            }
        ));
        match &b.stmts[2].kind {
            StmtKind::Expr(e) => assert!(matches!(&e.kind, ExprKind::If { else_: Some(_), .. })),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn struct_literal_not_in_condition() {
        let m = parse_ok("fn f() { if x == Foo { 1 } else { 2 }\n let p = Point { x: 1, y }\n }");
        let b = body(&m);
        match &b.stmts[1].kind {
            StmtKind::Let { init, .. } => match &init.kind {
                ExprKind::StructLit { fields, .. } => {
                    assert_eq!(fields.len(), 2);
                    assert!(matches!(fields[1].value.kind, ExprKind::Name(ref n) if n == "y"));
                }
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn match_and_patterns() {
        let m = parse_ok(
            "fn f() {\n match s {\n  Circle(r) if r > 0.0 => r,\n  Shape.Square(Point { x, y: _, .. }) => x\n  (1 | 2, \"a\") => 0.0,\n  None | _ => { 1.0 }\n }\n}",
        );
        match &body(&m).stmts[0].kind {
            StmtKind::Expr(e) => match &e.kind {
                ExprKind::Match { arms, .. } => {
                    assert_eq!(arms.len(), 4);
                    assert!(arms[0].guard.is_some());
                    assert!(matches!(
                        arms[1].pattern.kind,
                        PatternKind::Variant {
                            enum_name: Some(_),
                            ..
                        }
                    ));
                    assert!(matches!(arms[3].pattern.kind, PatternKind::Or(_)));
                }
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn interpolation() {
        let m = parse_ok("fn f() { \"x = ${x + 1}, y = ${p.y}!\" }");
        match &body(&m).stmts[0].kind {
            StmtKind::Expr(e) => match &e.kind {
                ExprKind::Interp(parts) => assert_eq!(parts.len(), 5),
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn lambdas_and_types() {
        parse_ok(
            "fn compose[A, B, C](f: fn(A) -> B, g: fn(B) -> C) -> fn(A) -> C {\n fn(x) => g(f(x))\n}\n\
             fn pairs() -> [(Int, String)] { [(1, \"a\")] }\n\
             fn unit() -> () { () }",
        );
    }

    #[test]
    fn reports_and_recovers() {
        let errs = errors("fn f( { }\nfn g() { let = 1 }\nfn h() { 1 < 2 < 3 }\nlet x = 1\n");
        assert!(
            errs.iter().any(|e| e.contains("expected a parameter name")),
            "{errs:?}"
        );
        assert!(
            errs.iter().any(|e| e.contains("expected a pattern")),
            "{errs:?}"
        );
        assert!(
            errs.iter().any(|e| e.contains("cannot be chained")),
            "{errs:?}"
        );
        assert!(
            errs.iter().any(|e| e.contains("inside functions")),
            "{errs:?}"
        );
    }

    #[test]
    fn missing_separator() {
        let errs = errors("fn f() { let x = 1 let y = 2 }");
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].contains("`;` or a line break"));
    }
}
