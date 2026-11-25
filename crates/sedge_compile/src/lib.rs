//! Lowers checked syntax trees to register bytecode.
//!
//! Registers are allocated like a stack: locals take the next free register
//! for the rest of their scope, and temporaries above them are released at
//! the end of each statement. A call puts its arguments in the registers
//! at the top, which become the callee's first registers.
//!
//! Locals that a closure captures and that can be reassigned (`var`) live
//! in heap cells, so the closure and the function share them. Other
//! captured locals are copied into the closure when it is created.

use sedge_check::{AdtKind, Analysis, CallRes, FnId, FnSource, LocalId, PatRes, Res, Ty, Typed};
use sedge_syntax::ast::*;
use sedge_syntax::{SourceMap, Span};
use sedge_vm::{Const, Op, Program, Proto, Reg, TypeInfo};
use std::collections::HashMap;

#[derive(Debug)]
pub struct CompileError {
    pub message: String,
    pub span: Span,
}

/// Compiles a checked program. The analysis must have no errors.
pub fn compile(a: &Analysis) -> Result<Program, CompileError> {
    assert!(!a.has_errors(), "compile called on a program with errors");
    let mut c = Compiler {
        typed: &a.typed,
        modules: &a.modules,
        map: &a.map,
        program: Program::default(),
        fn_proto: HashMap::new(),
        fn_native: HashMap::new(),
        native_index: HashMap::new(),
        consts: HashMap::new(),
        wrappers: HashMap::new(),
        variant_wrappers: HashMap::new(),
    };
    c.run()?;
    Ok(c.program)
}

#[derive(PartialEq, Eq, Hash)]
enum ConstKey {
    Int(i64),
    Float(u64),
    Str(String),
}

struct Compiler<'a> {
    typed: &'a Typed,
    modules: &'a [Module],
    map: &'a SourceMap,
    program: Program,
    fn_proto: HashMap<FnId, u32>,
    fn_native: HashMap<FnId, u32>,
    native_index: HashMap<String, u32>,
    consts: HashMap<ConstKey, u32>,
    /// Closures that call a native function, for natives used as values.
    wrappers: HashMap<FnId, u32>,
    variant_wrappers: HashMap<(u32, u32), u32>,
}

struct Loop {
    start: usize,
    breaks: Vec<usize>,
    continues: Vec<usize>,
}

struct Builder {
    code: Vec<Op>,
    lines: Vec<u32>,
    line: u32,
    next: u32,
    max: u32,
    locals: HashMap<LocalId, Reg>,
    captures: HashMap<LocalId, u16>,
    loops: Vec<Loop>,
    file: String,
}

impl Builder {
    fn new(file: String) -> Builder {
        Builder {
            code: Vec::new(),
            lines: Vec::new(),
            line: 0,
            next: 0,
            max: 0,
            locals: HashMap::new(),
            captures: HashMap::new(),
            loops: Vec::new(),
            file,
        }
    }

    fn emit(&mut self, op: Op) -> usize {
        self.code.push(op);
        self.lines.push(self.line);
        self.code.len() - 1
    }

    fn alloc(&mut self) -> Reg {
        let r = self.next;
        self.next += 1;
        self.max = self.max.max(self.next);
        r as Reg
    }

    fn alloc_n(&mut self, n: usize) -> Reg {
        let r = self.next;
        self.next += n as u32;
        self.max = self.max.max(self.next);
        r as Reg
    }

    fn here(&self) -> usize {
        self.code.len()
    }

    /// Points the jump at `at` to the current position.
    fn patch(&mut self, at: usize) {
        let target = self.code.len();
        self.patch_to(at, target);
    }

    fn patch_to(&mut self, at: usize, target: usize) {
        let off = target as i32 - (at as i32 + 1);
        match &mut self.code[at] {
            Op::Jump { offset }
            | Op::JumpIf { offset, .. }
            | Op::JumpIfNot { offset, .. }
            | Op::JumpIfNotLtI { offset, .. }
            | Op::JumpIfNotLeI { offset, .. }
            | Op::JumpIfNotTag { offset, .. } => *offset = off,
            other => panic!("patching a non-jump {other:?}"),
        }
    }

    fn jump_back(&mut self, target: usize) {
        let at = self.emit(Op::Jump { offset: 0 });
        self.patch_to(at, target);
    }

    fn finish(self, name: String, arity: u16, builtin: bool) -> Proto {
        Proto {
            name,
            file: self.file,
            arity,
            num_regs: self.max.max(1) as u16,
            code: self.code,
            lines: self.lines,
            builtin,
        }
    }
}

fn int_kind(t: &Ty) -> Kind {
    match t {
        Ty::Int => Kind::Int,
        Ty::Float => Kind::Float,
        Ty::Str => Kind::Str,
        _ => Kind::Other,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Int,
    Float,
    Str,
    Other,
}

impl<'a> Compiler<'a> {
    fn run(&mut self) -> Result<(), CompileError> {
        let t = self.typed;
        self.program.types = t
            .adts
            .iter()
            .map(|a| TypeInfo {
                name: a.name.clone(),
                fields: match &a.kind {
                    AdtKind::Struct(fs) => fs.iter().map(|f| f.name.clone()).collect(),
                    AdtKind::Enum(_) => Vec::new(),
                },
                variants: a.variants().iter().map(|v| v.name.clone()).collect(),
            })
            .collect();
        self.program.option_adt = t.option_adt.unwrap_or(0);
        self.program.result_adt = t.result_adt.unwrap_or(0);
        // Number every function first so calls can refer to later ones.
        for (id, f) in t.fns.iter().enumerate() {
            if f.native {
                let idx = self.native(&f.name);
                self.fn_native.insert(id as FnId, idx);
            } else {
                let idx = self.program.protos.len() as u32;
                self.program.protos.push(Proto::default());
                self.fn_proto.insert(id as FnId, idx);
            }
        }
        for (id, f) in t.fns.iter().enumerate() {
            if f.native {
                continue;
            }
            let decl = self.fn_decl(f.source);
            let mut b = Builder::new(self.file_of(f.span));
            for &l in &f.param_locals {
                let r = b.alloc();
                b.locals.insert(l, r);
            }
            self.box_params(&mut b, &f.param_locals);
            let body = decl.body.as_ref().unwrap();
            b.line = self.line(body.span);
            self.tail_block(&mut b, body, f.ret == Ty::Unit)?;
            self.check_regs(&b, f.span)?;
            let proto = b.finish(f.name.clone(), f.params.len() as u16, f.builtin);
            let idx = self.fn_proto[&(id as FnId)];
            self.program.protos[idx as usize] = proto;
        }
        for test in &t.tests {
            let Item::Test(decl) = &self.modules[test.module].items[test.item] else {
                unreachable!()
            };
            let mut b = Builder::new(self.file_of(test.span));
            b.line = self.line(decl.span);
            self.block(&mut b, &decl.body, None)?;
            b.emit(Op::ReturnUnit);
            self.check_regs(&b, test.span)?;
            let idx = self.program.protos.len() as u32;
            self.program
                .protos
                .push(b.finish(format!("test \"{}\"", test.name), 0, false));
            self.program.tests.push((test.name.clone(), idx));
        }
        if let Some(main) = t.main_fn() {
            if let Some(&p) = self.fn_proto.get(&main) {
                self.program.main = Some(p);
            }
        }
        Ok(())
    }

    fn check_regs(&self, b: &Builder, span: Span) -> Result<(), CompileError> {
        if b.max > u16::MAX as u32 - 16 {
            return Err(CompileError {
                message: "this function needs too many registers".into(),
                span,
            });
        }
        Ok(())
    }

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

    fn file_of(&self, span: Span) -> String {
        self.map.lookup(span.lo).name.clone()
    }

    fn line(&self, span: Span) -> u32 {
        self.map.lookup(span.lo).line_col(span.lo).line
    }

    fn native(&mut self, name: &str) -> u32 {
        if let Some(&i) = self.native_index.get(name) {
            return i;
        }
        let i = self.program.natives.len() as u32;
        self.program.natives.push(name.to_string());
        self.native_index.insert(name.to_string(), i);
        i
    }

    fn constant(&mut self, c: Const) -> u32 {
        let key = match &c {
            Const::Int(i) => ConstKey::Int(*i),
            Const::Float(f) => ConstKey::Float(f.to_bits()),
            Const::Str(s) => ConstKey::Str(s.clone()),
        };
        if let Some(&i) = self.consts.get(&key) {
            return i;
        }
        let i = self.program.consts.len() as u32;
        self.program.consts.push(c);
        self.consts.insert(key, i);
        i
    }

    fn box_params(&self, b: &mut Builder, params: &[LocalId]) {
        for &l in params {
            if self.typed.locals[l as usize].boxed {
                let r = b.locals[&l];
                b.emit(Op::NewCell { dst: r, src: r });
            }
        }
    }

    fn ty(&self, id: NodeId) -> &'a Ty {
        self.typed.ty_of(id)
    }

    fn is_boxed(&self, l: LocalId) -> bool {
        self.typed.locals[l as usize].boxed
    }

    // ----- locals ------------------------------------------------------------

    /// Declares a local in a fresh register and stores `src` into it.
    fn declare_local(&self, b: &mut Builder, l: LocalId, src: Reg) {
        let r = b.alloc();
        if self.is_boxed(l) {
            b.emit(Op::NewCell { dst: r, src });
        } else {
            b.emit(Op::Move { dst: r, src });
        }
        b.locals.insert(l, r);
    }

    /// The register of a local's raw slot (its cell, if boxed), loading a
    /// capture into a temporary if needed.
    fn local_slot(&self, b: &mut Builder, l: LocalId) -> Reg {
        if let Some(&r) = b.locals.get(&l) {
            return r;
        }
        let idx = b.captures[&l];
        let t = b.alloc();
        b.emit(Op::GetCapture { dst: t, idx });
        t
    }

    fn read_local(&self, b: &mut Builder, l: LocalId, dst: Option<Reg>) -> Reg {
        let slot = self.local_slot(b, l);
        if self.is_boxed(l) {
            let t = dst.unwrap_or_else(|| b.alloc());
            b.emit(Op::CellGet { dst: t, cell: slot });
            return t;
        }
        match dst {
            Some(d) if d != slot => {
                b.emit(Op::Move { dst: d, src: slot });
                d
            }
            _ => slot,
        }
    }

    fn write_local(&self, b: &mut Builder, l: LocalId, src: Reg) {
        let slot = self.local_slot(b, l);
        if self.is_boxed(l) {
            b.emit(Op::CellSet { cell: slot, src });
        } else if slot != src {
            b.emit(Op::Move { dst: slot, src });
        }
    }

    fn dest(&self, b: &mut Builder, dst: Option<Reg>) -> Reg {
        dst.unwrap_or_else(|| b.alloc())
    }

    // ----- statements ------------------------------------------------------

    fn block(
        &mut self,
        b: &mut Builder,
        block: &Block,
        dst: Option<Reg>,
    ) -> Result<Reg, CompileError> {
        let target = self.dest(b, dst);
        let saved = b.next;
        let n = block.stmts.len();
        let mut produced = false;
        for (i, s) in block.stmts.iter().enumerate() {
            b.line = self.line(s.span);
            let last = i + 1 == n;
            match &s.kind {
                StmtKind::Expr(e) if last => {
                    self.expr(b, e, Some(target))?;
                    produced = true;
                }
                StmtKind::Let { .. } => {
                    self.stmt(b, s)?;
                }
                _ => {
                    let keep = b.next;
                    self.stmt(b, s)?;
                    b.next = keep;
                }
            }
        }
        if !produced {
            b.emit(Op::LoadUnit { dst: target });
        }
        b.next = saved;
        Ok(target)
    }

    fn stmt(&mut self, b: &mut Builder, s: &Stmt) -> Result<(), CompileError> {
        match &s.kind {
            StmtKind::Let { pattern, init, .. } => {
                if let (PatternKind::Name(_), Some(PatRes::Bind(l))) =
                    (&pattern.kind, self.typed.patterns.get(&pattern.id))
                {
                    // Compile straight into the new local's register.
                    let r = b.alloc();
                    self.expr(b, init, Some(r))?;
                    if self.is_boxed(*l) {
                        b.emit(Op::NewCell { dst: r, src: r });
                    }
                    b.locals.insert(*l, r);
                    return Ok(());
                }
                let keep = b.next;
                let v = self.expr(b, init, None)?;
                // Bindings go above the temporaries; they stay allocated.
                let mut fails = Vec::new();
                self.pattern(b, pattern, v, &mut fails)?;
                debug_assert!(fails.is_empty(), "irrefutable pattern emitted tests");
                let _ = keep;
            }
            StmtKind::Assign { target, op, value } => self.assign(b, target, *op, value)?,
            StmtKind::While { cond, body } => {
                let start = b.here();
                let exit = self.branch_unless(b, cond)?;
                b.loops.push(Loop {
                    start,
                    breaks: Vec::new(),
                    continues: Vec::new(),
                });
                let keep = b.next;
                self.block(b, body, None)?;
                b.next = keep;
                b.jump_back(start);
                let lp = b.loops.pop().unwrap();
                b.patch(exit);
                for at in lp.breaks {
                    b.patch(at);
                }
                for at in lp.continues {
                    b.patch_to(at, start);
                }
            }
            StmtKind::For {
                pattern,
                iter,
                body,
            } => self.for_loop(b, pattern, iter, body)?,
            StmtKind::Return(value) => match value {
                Some(v) => {
                    let r = self.expr(b, v, None)?;
                    b.emit(Op::Return { src: r });
                }
                None => {
                    b.emit(Op::ReturnUnit);
                }
            },
            StmtKind::Break => {
                let at = b.emit(Op::Jump { offset: 0 });
                b.loops.last_mut().unwrap().breaks.push(at);
            }
            StmtKind::Continue => {
                let at = b.emit(Op::Jump { offset: 0 });
                b.loops.last_mut().unwrap().continues.push(at);
            }
            StmtKind::Expr(e) => {
                self.expr(b, e, None)?;
            }
        }
        Ok(())
    }

    fn for_loop(
        &mut self,
        b: &mut Builder,
        pattern: &Pattern,
        iter: &Expr,
        body: &Block,
    ) -> Result<(), CompileError> {
        let saved = b.next;
        let is_range = matches!(self.ty(iter.id), Ty::Range);
        let (counter, end, arr) = if is_range {
            let counter = b.alloc();
            let end = b.alloc();
            match &iter.kind {
                ExprKind::Range { lo, hi, inclusive } => {
                    self.expr(b, lo, Some(counter))?;
                    self.expr(b, hi, Some(end))?;
                    if *inclusive {
                        b.emit(Op::AddImm {
                            dst: end,
                            a: end,
                            value: 1,
                        });
                    }
                }
                _ => {
                    let r = self.expr(b, iter, None)?;
                    b.emit(Op::GetField {
                        dst: counter,
                        obj: r,
                        idx: 0,
                    });
                    b.emit(Op::GetField {
                        dst: end,
                        obj: r,
                        idx: 1,
                    });
                }
            }
            (counter, end, None)
        } else {
            let arr = b.alloc();
            self.expr(b, iter, Some(arr))?;
            let counter = b.alloc();
            b.emit(Op::LoadInt {
                dst: counter,
                value: 0,
            });
            let end = b.alloc();
            (counter, end, Some(arr))
        };
        let start = b.here();
        if let Some(arr) = arr {
            b.emit(Op::Len { dst: end, obj: arr });
        }
        let exit = b.emit(Op::JumpIfNotLtI {
            a: counter,
            b: end,
            offset: 0,
        });
        let keep = b.next;
        let elem = b.alloc();
        match arr {
            Some(arr) => b.emit(Op::Index {
                dst: elem,
                obj: arr,
                idx: counter,
            }),
            None => b.emit(Op::Move {
                dst: elem,
                src: counter,
            }),
        };
        let mut fails = Vec::new();
        self.pattern(b, pattern, elem, &mut fails)?;
        b.loops.push(Loop {
            start,
            breaks: Vec::new(),
            continues: Vec::new(),
        });
        self.block(b, body, None)?;
        let step = b.here();
        b.emit(Op::AddImm {
            dst: counter,
            a: counter,
            value: 1,
        });
        b.jump_back(start);
        let lp = b.loops.pop().unwrap();
        b.patch(exit);
        for at in lp.breaks {
            b.patch(at);
        }
        for at in lp.continues {
            b.patch_to(at, step);
        }
        let _ = lp.start;
        b.next = keep;
        b.next = saved;
        Ok(())
    }

    fn assign(
        &mut self,
        b: &mut Builder,
        target: &Expr,
        op: AssignOp,
        value: &Expr,
    ) -> Result<(), CompileError> {
        let kind = int_kind(self.ty(target.id));
        match &target.kind {
            ExprKind::Name(_) => {
                let Some(Res::Local(l)) = self.typed.names.get(&target.id).copied() else {
                    unreachable!("assignment to a non-local")
                };
                if op == AssignOp::Set {
                    if !self.is_boxed(l) && b.locals.contains_key(&l) {
                        let r = b.locals[&l];
                        // Compute into a temporary first if the value reads
                        // the variable through a closure or patterns; a
                        // direct write is safe for plain expressions.
                        let v = self.expr(b, value, None)?;
                        if v != r {
                            b.emit(Op::Move { dst: r, src: v });
                        }
                    } else {
                        let v = self.expr(b, value, None)?;
                        self.write_local(b, l, v);
                    }
                } else {
                    let cur = self.read_local(b, l, None);
                    let v = self.expr(b, value, None)?;
                    let t = b.alloc();
                    self.arith(b, op, kind, t, cur, v);
                    self.write_local(b, l, t);
                }
            }
            ExprKind::Field { base, .. } => {
                let idx = self.typed.fields[&target.id] as u16;
                let obj = self.expr(b, base, None)?;
                let v = if op == AssignOp::Set {
                    self.expr(b, value, None)?
                } else {
                    let cur = b.alloc();
                    b.emit(Op::GetField { dst: cur, obj, idx });
                    let v = self.expr(b, value, None)?;
                    self.arith(b, op, kind, cur, cur, v);
                    cur
                };
                b.emit(Op::SetField { obj, idx, src: v });
            }
            ExprKind::Index { base, index } => {
                let obj = self.expr(b, base, None)?;
                let i = self.expr(b, index, None)?;
                let v = if op == AssignOp::Set {
                    self.expr(b, value, None)?
                } else {
                    let cur = b.alloc();
                    b.emit(Op::Index {
                        dst: cur,
                        obj,
                        idx: i,
                    });
                    let v = self.expr(b, value, None)?;
                    self.arith(b, op, kind, cur, cur, v);
                    cur
                };
                b.emit(Op::SetIndex {
                    obj,
                    idx: i,
                    src: v,
                });
            }
            _ => unreachable!("checker rejects other assignment targets"),
        }
        Ok(())
    }

    fn arith(&self, b: &mut Builder, op: AssignOp, kind: Kind, dst: Reg, x: Reg, y: Reg) {
        let bin = match op {
            AssignOp::Add => BinOp::Add,
            AssignOp::Sub => BinOp::Sub,
            AssignOp::Mul => BinOp::Mul,
            AssignOp::Div => BinOp::Div,
            AssignOp::Rem => BinOp::Rem,
            AssignOp::Set => unreachable!(),
        };
        b.emit(arith_op(bin, kind, dst, x, y));
    }

    /// Emits a jump taken when `cond` is false, fusing Int comparisons
    /// into the jump. Returns the jump to patch.
    fn branch_unless(&mut self, b: &mut Builder, cond: &Expr) -> Result<usize, CompileError> {
        if let ExprKind::Binary { op, lhs, rhs } = &cond.kind {
            if matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge)
                && int_kind(self.ty(lhs.id)) == Kind::Int
            {
                let x = self.expr(b, lhs, None)?;
                let y = self.expr(b, rhs, None)?;
                let (p, q) = if matches!(op, BinOp::Gt | BinOp::Ge) {
                    (y, x)
                } else {
                    (x, y)
                };
                let jump = if matches!(op, BinOp::Lt | BinOp::Gt) {
                    Op::JumpIfNotLtI {
                        a: p,
                        b: q,
                        offset: 0,
                    }
                } else {
                    Op::JumpIfNotLeI {
                        a: p,
                        b: q,
                        offset: 0,
                    }
                };
                return Ok(b.emit(jump));
            }
        }
        let c = self.expr(b, cond, None)?;
        Ok(b.emit(Op::JumpIfNot { cond: c, offset: 0 }))
    }

    /// Compiles an expression whose value is returned, emitting `return`
    /// in each branch of `if` and `match` instead of joining first.
    fn tail(&mut self, b: &mut Builder, e: &Expr, unit: bool) -> Result<(), CompileError> {
        match &e.kind {
            ExprKind::If {
                cond,
                then,
                else_: Some(els),
            } => {
                b.line = self.line(e.span);
                let to_else = self.branch_unless(b, cond)?;
                let saved = b.next;
                self.tail_block(b, then, unit)?;
                b.next = saved;
                b.patch(to_else);
                self.tail(b, els, unit)?;
                b.next = saved;
            }
            ExprKind::Block(block) => self.tail_block(b, block, unit)?,
            _ => {
                let r = self.expr(b, e, None)?;
                if unit {
                    b.emit(Op::ReturnUnit);
                } else {
                    b.emit(Op::Return { src: r });
                }
            }
        }
        Ok(())
    }

    fn tail_block(
        &mut self,
        b: &mut Builder,
        block: &Block,
        unit: bool,
    ) -> Result<(), CompileError> {
        let saved = b.next;
        let n = block.stmts.len();
        for (i, s) in block.stmts.iter().enumerate() {
            b.line = self.line(s.span);
            match &s.kind {
                StmtKind::Expr(e) if i + 1 == n => {
                    self.tail(b, e, unit)?;
                    b.next = saved;
                    return Ok(());
                }
                StmtKind::Let { .. } => self.stmt(b, s)?,
                _ => {
                    let keep = b.next;
                    self.stmt(b, s)?;
                    b.next = keep;
                }
            }
        }
        b.next = saved;
        if unit {
            b.emit(Op::ReturnUnit);
        } else {
            let r = b.alloc();
            b.emit(Op::LoadUnit { dst: r });
            b.emit(Op::Return { src: r });
            b.next = saved;
        }
        Ok(())
    }

    // ----- expressions -----------------------------------------------------

    fn load_int(&mut self, b: &mut Builder, dst: Reg, v: i64) {
        match i32::try_from(v) {
            Ok(i) => b.emit(Op::LoadInt { dst, value: i }),
            Err(_) => {
                let idx = self.constant(Const::Int(v));
                b.emit(Op::LoadConst { dst, idx })
            }
        };
    }

    fn load_str(&mut self, b: &mut Builder, dst: Reg, s: &str) {
        let idx = self.constant(Const::Str(s.to_string()));
        b.emit(Op::LoadConst { dst, idx });
    }

    /// Compiles `e` and returns the register holding its value: `dst` if
    /// given, otherwise any register (possibly a local's).
    fn expr(&mut self, b: &mut Builder, e: &Expr, dst: Option<Reg>) -> Result<Reg, CompileError> {
        let line = self.line(e.span);
        if line != 0 {
            b.line = line;
        }
        Ok(match &e.kind {
            ExprKind::Int(v) => {
                let d = self.dest(b, dst);
                self.load_int(b, d, *v);
                d
            }
            ExprKind::Float(f) => {
                let d = self.dest(b, dst);
                let idx = self.constant(Const::Float(*f));
                b.emit(Op::LoadConst { dst: d, idx });
                d
            }
            ExprKind::Str(s) => {
                let d = self.dest(b, dst);
                self.load_str(b, d, s);
                d
            }
            ExprKind::Bool(v) => {
                let d = self.dest(b, dst);
                b.emit(Op::LoadBool { dst: d, value: *v });
                d
            }
            ExprKind::Unit => {
                let d = self.dest(b, dst);
                b.emit(Op::LoadUnit { dst: d });
                d
            }
            ExprKind::Error => unreachable!("error node in a checked program"),
            ExprKind::Interp(parts) => self.interp(b, parts, dst)?,
            ExprKind::Name(_) | ExprKind::SelfValue => match self.typed.names.get(&e.id).copied() {
                Some(Res::Local(l)) => self.read_local(b, l, dst),
                Some(Res::Fn(f)) => {
                    let d = self.dest(b, dst);
                    let func = self.fn_value(f);
                    b.emit(Op::MakeClosure {
                        dst: d,
                        func,
                        base: 0,
                        count: 0,
                    });
                    d
                }
                Some(Res::Variant(adt, v)) => self.variant_value(b, adt, v, dst),
                None => unreachable!("unresolved name"),
            },
            ExprKind::Field { base, .. } => {
                if let Some(Res::Variant(adt, v)) = self.typed.names.get(&e.id).copied() {
                    return Ok(self.variant_value(b, adt, v, dst));
                }
                let idx = self.typed.fields[&e.id] as u16;
                let obj = self.expr(b, base, None)?;
                let d = self.dest(b, dst);
                b.emit(Op::GetField { dst: d, obj, idx });
                d
            }
            ExprKind::TupleField { base, index, .. } => {
                let obj = self.expr(b, base, None)?;
                let d = self.dest(b, dst);
                b.emit(Op::GetField {
                    dst: d,
                    obj,
                    idx: *index as u16,
                });
                d
            }
            ExprKind::Call { callee, args } => self.call(b, e, callee, args, dst)?,
            ExprKind::MethodCall { recv, args, .. } => self.method_call(b, e, recv, args, dst)?,
            ExprKind::Index { base, index } => {
                let obj = self.expr(b, base, None)?;
                let i = self.expr(b, index, None)?;
                let d = self.dest(b, dst);
                b.emit(Op::Index {
                    dst: d,
                    obj,
                    idx: i,
                });
                d
            }
            ExprKind::Binary { op, lhs, rhs } => self.binary(b, *op, lhs, rhs, dst)?,
            ExprKind::Unary { op, expr } => {
                let v = self.expr(b, expr, None)?;
                let d = self.dest(b, dst);
                match op {
                    UnOp::Not => b.emit(Op::Not { dst: d, src: v }),
                    UnOp::Neg => match int_kind(self.ty(expr.id)) {
                        Kind::Int => b.emit(Op::NegI { dst: d, src: v }),
                        Kind::Float => b.emit(Op::NegF { dst: d, src: v }),
                        _ => b.emit(Op::NegG { dst: d, src: v }),
                    },
                };
                d
            }
            ExprKind::If { cond, then, else_ } => {
                let d = self.dest(b, dst);
                let to_else = self.branch_unless(b, cond)?;
                self.block(b, then, Some(d))?;
                match else_ {
                    Some(els) => {
                        let to_end = b.emit(Op::Jump { offset: 0 });
                        b.patch(to_else);
                        self.expr(b, els, Some(d))?;
                        b.patch(to_end);
                    }
                    None => {
                        let to_end = b.emit(Op::Jump { offset: 0 });
                        b.patch(to_else);
                        b.emit(Op::LoadUnit { dst: d });
                        b.patch(to_end);
                    }
                }
                d
            }
            ExprKind::Match { scrutinee, arms } => self.match_expr(b, scrutinee, arms, dst)?,
            ExprKind::Block(block) => self.block(b, block, dst)?,
            ExprKind::Array(elems) => {
                let base = self.sequence(b, elems)?;
                let d = self.dest(b, dst);
                b.emit(Op::NewArray {
                    dst: d,
                    base,
                    count: elems.len() as u16,
                });
                d
            }
            ExprKind::Tuple(elems) => {
                let base = self.sequence(b, elems)?;
                let d = self.dest(b, dst);
                b.emit(Op::NewTuple {
                    dst: d,
                    base,
                    count: elems.len() as u16,
                });
                d
            }
            ExprKind::StructLit { fields, .. } => {
                let Ty::Adt(adt, _) = self.ty(e.id) else {
                    unreachable!("struct literal type")
                };
                let defs = self.typed.adts[*adt as usize].fields();
                let base = b.alloc_n(defs.len());
                for f in fields {
                    let i = defs.iter().position(|d| d.name == f.name.name).unwrap();
                    self.expr(b, &f.value, Some(base + i as Reg))?;
                }
                let d = self.dest(b, dst);
                b.emit(Op::NewStruct {
                    dst: d,
                    adt: *adt,
                    base,
                    count: defs.len() as u16,
                });
                d
            }
            ExprKind::Lambda { body, .. } => self.lambda(b, e, body, dst)?,
            ExprKind::Range { lo, hi, inclusive } => {
                let l = self.expr(b, lo, None)?;
                let h = b.alloc();
                self.expr(b, hi, Some(h))?;
                if *inclusive {
                    b.emit(Op::AddImm {
                        dst: h,
                        a: h,
                        value: 1,
                    });
                }
                let d = self.dest(b, dst);
                b.emit(Op::NewRange {
                    dst: d,
                    lo: l,
                    hi: h,
                });
                d
            }
            ExprKind::Try(inner) => {
                let v = self.expr(b, inner, None)?;
                let d = self.dest(b, dst);
                // Tag 0 is `Some` for Option and `Ok` for Result.
                let fail = b.emit(Op::JumpIfNotTag {
                    obj: v,
                    tag: 0,
                    offset: 0,
                });
                b.emit(Op::GetField {
                    dst: d,
                    obj: v,
                    idx: 0,
                });
                let done = b.emit(Op::Jump { offset: 0 });
                b.patch(fail);
                b.emit(Op::Return { src: v });
                b.patch(done);
                d
            }
        })
    }

    /// Compiles expressions into consecutive fresh registers.
    fn sequence(&mut self, b: &mut Builder, elems: &[Expr]) -> Result<Reg, CompileError> {
        let base = b.alloc_n(elems.len());
        for (i, x) in elems.iter().enumerate() {
            self.expr(b, x, Some(base + i as Reg))?;
        }
        Ok(base)
    }

    fn interp(
        &mut self,
        b: &mut Builder,
        parts: &[InterpPart],
        dst: Option<Reg>,
    ) -> Result<Reg, CompileError> {
        let d = self.dest(b, dst);
        let mut first = true;
        for part in parts {
            let piece = b.alloc();
            match part {
                InterpPart::Lit(s) => self.load_str(b, piece, s),
                InterpPart::Expr(x) => {
                    let v = self.expr(b, x, None)?;
                    b.emit(Op::ToStr { dst: piece, src: v });
                }
            }
            if first {
                b.emit(Op::Move { dst: d, src: piece });
                first = false;
            } else {
                b.emit(Op::Concat {
                    dst: d,
                    a: d,
                    b: piece,
                });
            }
        }
        if first {
            self.load_str(b, d, "");
        }
        Ok(d)
    }

    fn binary(
        &mut self,
        b: &mut Builder,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        dst: Option<Reg>,
    ) -> Result<Reg, CompileError> {
        if matches!(op, BinOp::And | BinOp::Or) {
            let d = self.dest(b, dst);
            self.expr(b, lhs, Some(d))?;
            let skip = if op == BinOp::And {
                b.emit(Op::JumpIfNot { cond: d, offset: 0 })
            } else {
                b.emit(Op::JumpIf { cond: d, offset: 0 })
            };
            self.expr(b, rhs, Some(d))?;
            b.patch(skip);
            return Ok(d);
        }
        let kind = int_kind(self.ty(lhs.id));
        // `x + 1` on Ints becomes a single instruction.
        if matches!(op, BinOp::Add | BinOp::Sub) && kind == Kind::Int {
            if let ExprKind::Int(v) = rhs.kind {
                let v = if op == BinOp::Sub {
                    v.checked_neg()
                } else {
                    Some(v)
                };
                if let Some(imm) = v.and_then(|v| i32::try_from(v).ok()) {
                    let x = self.expr(b, lhs, None)?;
                    let d = self.dest(b, dst);
                    b.emit(Op::AddImm {
                        dst: d,
                        a: x,
                        value: imm,
                    });
                    return Ok(d);
                }
            }
        }
        let x = self.expr(b, lhs, None)?;
        let y = self.expr(b, rhs, None)?;
        let d = self.dest(b, dst);
        let instr = match op {
            BinOp::Eq => Op::Eq { dst: d, a: x, b: y },
            BinOp::Ne => Op::Ne { dst: d, a: x, b: y },
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                // `a > b` is `b < a`.
                let (p, q) = if matches!(op, BinOp::Gt | BinOp::Ge) {
                    (y, x)
                } else {
                    (x, y)
                };
                let strict = matches!(op, BinOp::Lt | BinOp::Gt);
                match (kind, strict) {
                    (Kind::Int, true) => Op::LtI { dst: d, a: p, b: q },
                    (Kind::Int, false) => Op::LeI { dst: d, a: p, b: q },
                    (Kind::Float, true) => Op::LtF { dst: d, a: p, b: q },
                    (Kind::Float, false) => Op::LeF { dst: d, a: p, b: q },
                    (_, true) => Op::Lt { dst: d, a: p, b: q },
                    (_, false) => Op::Le { dst: d, a: p, b: q },
                }
            }
            BinOp::BitAnd => Op::BitAnd { dst: d, a: x, b: y },
            BinOp::BitOr => Op::BitOr { dst: d, a: x, b: y },
            BinOp::BitXor => Op::BitXor { dst: d, a: x, b: y },
            BinOp::Shl => Op::Shl { dst: d, a: x, b: y },
            BinOp::Shr => Op::Shr { dst: d, a: x, b: y },
            _ => arith_op(op, kind, d, x, y),
        };
        b.emit(instr);
        Ok(d)
    }

    // ----- calls -------------------------------------------------------------

    /// Emits a call to a known function with arguments already in
    /// consecutive registers from `base`.
    fn emit_direct(&mut self, b: &mut Builder, f: FnId, base: Reg, argc: usize) {
        if let Some(&p) = self.fn_proto.get(&f) {
            b.emit(Op::CallDirect {
                func: p,
                base,
                argc: argc as u8,
            });
        } else {
            let n = self.fn_native[&f];
            b.emit(Op::CallNative {
                native: n,
                base,
                argc: argc as u8,
            });
        }
    }

    fn finish_call(&self, b: &mut Builder, base: Reg, dst: Option<Reg>) -> Reg {
        match dst {
            Some(d) if d != base => {
                b.emit(Op::Move { dst: d, src: base });
                d
            }
            _ => base,
        }
    }

    fn call(
        &mut self,
        b: &mut Builder,
        e: &Expr,
        callee: &Expr,
        args: &[Expr],
        dst: Option<Reg>,
    ) -> Result<Reg, CompileError> {
        match self.typed.calls.get(&e.id).copied() {
            Some(CallRes::Direct(f)) => {
                let base = self.sequence(b, args)?;
                self.emit_direct(b, f, base, args.len());
                Ok(self.finish_call(b, base, dst))
            }
            Some(CallRes::Variant(adt, tag)) => {
                let base = self.sequence(b, args)?;
                let d = self.dest(b, dst);
                b.emit(Op::NewVariant {
                    dst: d,
                    adt,
                    tag,
                    base,
                    count: args.len() as u16,
                });
                Ok(d)
            }
            _ => {
                let base = b.alloc_n(args.len() + 1);
                self.expr(b, callee, Some(base))?;
                for (i, a) in args.iter().enumerate() {
                    self.expr(b, a, Some(base + 1 + i as Reg))?;
                }
                b.emit(Op::Call {
                    base,
                    argc: args.len() as u8,
                });
                Ok(self.finish_call(b, base, dst))
            }
        }
    }

    fn method_call(
        &mut self,
        b: &mut Builder,
        e: &Expr,
        recv: &Expr,
        args: &[Expr],
        dst: Option<Reg>,
    ) -> Result<Reg, CompileError> {
        match self.typed.calls.get(&e.id).copied() {
            Some(CallRes::Direct(f)) => {
                let base = b.alloc_n(args.len() + 1);
                self.expr(b, recv, Some(base))?;
                for (i, a) in args.iter().enumerate() {
                    self.expr(b, a, Some(base + 1 + i as Reg))?;
                }
                self.emit_direct(b, f, base, args.len() + 1);
                Ok(self.finish_call(b, base, dst))
            }
            Some(CallRes::Static(f)) => {
                let base = self.sequence(b, args)?;
                self.emit_direct(b, f, base, args.len());
                Ok(self.finish_call(b, base, dst))
            }
            Some(CallRes::Variant(adt, tag)) => {
                let base = self.sequence(b, args)?;
                let d = self.dest(b, dst);
                b.emit(Op::NewVariant {
                    dst: d,
                    adt,
                    tag,
                    base,
                    count: args.len() as u16,
                });
                Ok(d)
            }
            Some(CallRes::FieldClosure(idx)) => {
                let obj = self.expr(b, recv, None)?;
                let base = b.alloc_n(args.len() + 1);
                b.emit(Op::GetField {
                    dst: base,
                    obj,
                    idx: idx as u16,
                });
                for (i, a) in args.iter().enumerate() {
                    self.expr(b, a, Some(base + 1 + i as Reg))?;
                }
                b.emit(Op::Call {
                    base,
                    argc: args.len() as u8,
                });
                Ok(self.finish_call(b, base, dst))
            }
            Some(CallRes::Indirect) | None => unreachable!("method call without resolution"),
        }
    }

    /// A closure that calls function `f`, for functions used as values.
    fn fn_value(&mut self, f: FnId) -> u32 {
        if let Some(&p) = self.fn_proto.get(&f) {
            return p;
        }
        if let Some(&p) = self.wrappers.get(&f) {
            return p;
        }
        let def = &self.typed.fns[f as usize];
        let n = def.params.len();
        let native = self.fn_native[&f];
        let mut b = Builder::new("<prelude>".into());
        b.alloc_n(n.max(1));
        b.emit(Op::CallNative {
            native,
            base: 0,
            argc: n as u8,
        });
        b.emit(Op::Return { src: 0 });
        let idx = self.program.protos.len() as u32;
        self.program
            .protos
            .push(b.finish(def.name.clone(), n as u16, true));
        self.wrappers.insert(f, idx);
        idx
    }

    fn variant_value(&mut self, b: &mut Builder, adt: u32, tag: u32, dst: Option<Reg>) -> Reg {
        let d = self.dest(b, dst);
        let n = self.typed.adts[adt as usize].variants()[tag as usize]
            .fields
            .len();
        if n == 0 {
            b.emit(Op::LoadTag { dst: d, adt, tag });
            return d;
        }
        let func = match self.variant_wrappers.get(&(adt, tag)) {
            Some(&p) => p,
            None => {
                let mut w = Builder::new("<prelude>".into());
                w.alloc_n(n + 1);
                w.emit(Op::NewVariant {
                    dst: n as Reg,
                    adt,
                    tag,
                    base: 0,
                    count: n as u16,
                });
                w.emit(Op::Return { src: n as Reg });
                let name = self.typed.adts[adt as usize].variants()[tag as usize]
                    .name
                    .clone();
                let idx = self.program.protos.len() as u32;
                self.program.protos.push(w.finish(name, n as u16, true));
                self.variant_wrappers.insert((adt, tag), idx);
                idx
            }
        };
        b.emit(Op::MakeClosure {
            dst: d,
            func,
            base: 0,
            count: 0,
        });
        d
    }

    fn lambda(
        &mut self,
        b: &mut Builder,
        e: &Expr,
        body: &Expr,
        dst: Option<Reg>,
    ) -> Result<Reg, CompileError> {
        let info = &self.typed.lambdas[&e.id];
        // Gather the captured slots into consecutive registers.
        let base = b.alloc_n(info.captures.len());
        for (i, &l) in info.captures.iter().enumerate() {
            let slot = self.local_slot(b, l);
            b.emit(Op::Move {
                dst: base + i as Reg,
                src: slot,
            });
        }
        let mut nb = Builder::new(self.file_of(e.span));
        nb.line = self.line(e.span);
        for &l in &info.params {
            let r = nb.alloc();
            nb.locals.insert(l, r);
        }
        for (i, &l) in info.captures.iter().enumerate() {
            nb.captures.insert(l, i as u16);
        }
        self.box_params(&mut nb, &info.params);
        let ret_unit = matches!(self.ty(e.id), Ty::Fn(_, r) if **r == Ty::Unit);
        self.tail(&mut nb, body, ret_unit)?;
        self.check_regs(&nb, e.span)?;
        let line = self.line(e.span);
        let name = format!("<lambda:{line}>");
        let idx = self.program.protos.len() as u32;
        self.program.protos.push(nb.finish(
            name,
            info.params.len() as u16,
            self.map.lookup(e.span.lo).name == sedge_check::PRELUDE_NAME,
        ));
        let d = self.dest(b, dst);
        b.emit(Op::MakeClosure {
            dst: d,
            func: idx,
            base,
            count: info.captures.len() as u16,
        });
        Ok(d)
    }

    // ----- match -------------------------------------------------------------

    fn match_expr(
        &mut self,
        b: &mut Builder,
        scrutinee: &Expr,
        arms: &[Arm],
        dst: Option<Reg>,
    ) -> Result<Reg, CompileError> {
        let d = self.dest(b, dst);
        let s = self.expr(b, scrutinee, None)?;
        let mut ends = Vec::new();
        for arm in arms {
            let saved = b.next;
            let mut fails = Vec::new();
            self.pattern(b, &arm.pattern, s, &mut fails)?;
            if let Some(g) = &arm.guard {
                let c = self.expr(b, g, None)?;
                fails.push(b.emit(Op::JumpIfNot { cond: c, offset: 0 }));
            }
            self.expr(b, &arm.body, Some(d))?;
            ends.push(b.emit(Op::Jump { offset: 0 }));
            for f in fails {
                b.patch(f);
            }
            b.next = saved;
        }
        let msg = self.constant(Const::Str("no match arm matched".into()));
        b.emit(Op::Panic { msg });
        for at in ends {
            b.patch(at);
        }
        Ok(d)
    }

    /// Emits tests that jump (via `fails`) when `v` does not match, and
    /// binds the pattern's names.
    fn pattern(
        &mut self,
        b: &mut Builder,
        p: &Pattern,
        v: Reg,
        fails: &mut Vec<usize>,
    ) -> Result<(), CompileError> {
        match &p.kind {
            PatternKind::Wild | PatternKind::Error => {}
            PatternKind::Name(_) => match self.typed.patterns.get(&p.id).copied() {
                Some(PatRes::Bind(l)) => {
                    if b.locals.contains_key(&l) {
                        // A later alternative of an or-pattern.
                        self.write_local(b, l, v);
                    } else {
                        self.declare_local(b, l, v);
                    }
                }
                Some(PatRes::Variant(_, tag)) => {
                    fails.push(b.emit(Op::JumpIfNotTag {
                        obj: v,
                        tag,
                        offset: 0,
                    }));
                }
                _ => {}
            },
            PatternKind::Int(i) => {
                let t = b.alloc();
                self.load_int(b, t, *i);
                b.emit(Op::Eq { dst: t, a: v, b: t });
                fails.push(b.emit(Op::JumpIfNot { cond: t, offset: 0 }));
            }
            PatternKind::Str(s) => {
                let t = b.alloc();
                self.load_str(b, t, s);
                b.emit(Op::Eq { dst: t, a: v, b: t });
                fails.push(b.emit(Op::JumpIfNot { cond: t, offset: 0 }));
            }
            PatternKind::Bool(true) => fails.push(b.emit(Op::JumpIfNot { cond: v, offset: 0 })),
            PatternKind::Bool(false) => fails.push(b.emit(Op::JumpIf { cond: v, offset: 0 })),
            PatternKind::Variant { args, .. } => {
                let Some(PatRes::Variant(_, tag)) = self.typed.patterns.get(&p.id).copied() else {
                    unreachable!()
                };
                fails.push(b.emit(Op::JumpIfNotTag {
                    obj: v,
                    tag,
                    offset: 0,
                }));
                for (i, a) in args.iter().enumerate() {
                    self.subpattern(b, a, v, i, fails)?;
                }
            }
            PatternKind::Struct { fields, .. } => {
                let Some(PatRes::Struct(adt)) = self.typed.patterns.get(&p.id).copied() else {
                    unreachable!()
                };
                let defs = self.typed.adts[adt as usize].fields();
                for (fname, fp) in fields {
                    let i = defs.iter().position(|d| d.name == fname.name).unwrap();
                    self.subpattern(b, fp, v, i, fails)?;
                }
            }
            PatternKind::Tuple(elems) => {
                for (i, x) in elems.iter().enumerate() {
                    self.subpattern(b, x, v, i, fails)?;
                }
            }
            PatternKind::Or(alts) => {
                let mut matched = Vec::new();
                for (i, alt) in alts.iter().enumerate() {
                    if i + 1 == alts.len() {
                        self.pattern(b, alt, v, fails)?;
                    } else {
                        let mut alt_fails = Vec::new();
                        self.pattern(b, alt, v, &mut alt_fails)?;
                        matched.push(b.emit(Op::Jump { offset: 0 }));
                        for f in alt_fails {
                            b.patch(f);
                        }
                    }
                }
                for at in matched {
                    b.patch(at);
                }
            }
        }
        Ok(())
    }

    fn subpattern(
        &mut self,
        b: &mut Builder,
        p: &Pattern,
        v: Reg,
        idx: usize,
        fails: &mut Vec<usize>,
    ) -> Result<(), CompileError> {
        if matches!(p.kind, PatternKind::Wild) {
            return Ok(());
        }
        let t = b.alloc();
        b.emit(Op::GetField {
            dst: t,
            obj: v,
            idx: idx as u16,
        });
        self.pattern(b, p, t, fails)
    }
}

fn arith_op(op: BinOp, kind: Kind, dst: Reg, a: Reg, b: Reg) -> Op {
    match (op, kind) {
        (BinOp::Add, Kind::Int) => Op::AddI { dst, a, b },
        (BinOp::Sub, Kind::Int) => Op::SubI { dst, a, b },
        (BinOp::Mul, Kind::Int) => Op::MulI { dst, a, b },
        (BinOp::Div, Kind::Int) => Op::DivI { dst, a, b },
        (BinOp::Rem, Kind::Int) => Op::RemI { dst, a, b },
        (BinOp::Add, Kind::Float) => Op::AddF { dst, a, b },
        (BinOp::Sub, Kind::Float) => Op::SubF { dst, a, b },
        (BinOp::Mul, Kind::Float) => Op::MulF { dst, a, b },
        (BinOp::Div, Kind::Float) => Op::DivF { dst, a, b },
        (BinOp::Rem, Kind::Float) => Op::RemF { dst, a, b },
        (BinOp::Add, Kind::Str) => Op::Concat { dst, a, b },
        (BinOp::Add, _) => Op::AddG { dst, a, b },
        (BinOp::Sub, _) => Op::SubG { dst, a, b },
        (BinOp::Mul, _) => Op::MulG { dst, a, b },
        (BinOp::Div, _) => Op::DivG { dst, a, b },
        (BinOp::Rem, _) => Op::RemG { dst, a, b },
        _ => unreachable!("not an arithmetic operator"),
    }
}
