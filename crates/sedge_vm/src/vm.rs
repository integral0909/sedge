//! The interpreter.

use crate::bytecode::{Const, Op, Program};
use crate::natives::{self, NativeFn};
use crate::ops;
use crate::value::{Gc, Heap, Object, Value};
use std::cmp::Ordering;
use std::fmt;
use std::io::{BufRead, Write};
use std::rc::Rc;
use std::time::Instant;

#[derive(Clone, Debug, PartialEq)]
pub struct TraceLine {
    pub function: String,
    pub file: String,
    pub line: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ErrorKind {
    Panic(String),
    /// `exit(code)` was called.
    Exit(i32),
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeError {
    pub kind: ErrorKind,
    /// Innermost call first.
    pub trace: Vec<TraceLine>,
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            ErrorKind::Panic(msg) => writeln!(f, "panic: {msg}")?,
            ErrorKind::Exit(code) => writeln!(f, "exit({code})")?,
        }
        for t in &self.trace {
            writeln!(f, "    at {} ({}:{})", t.function, t.file, t.line)?;
        }
        Ok(())
    }
}

/// What natives return on failure. The VM adds the stack trace.
pub enum NativeError {
    Panic(String),
    Exit(i32),
}

impl From<String> for NativeError {
    fn from(s: String) -> NativeError {
        NativeError::Panic(s)
    }
}

struct Frame {
    func: u32,
    ip: usize,
    base: usize,
    /// Stack slot that receives the return value.
    ret: usize,
    closure: Option<Gc>,
}

pub struct Vm {
    pub program: Rc<Program>,
    pub heap: Heap,
    stack: Vec<Value>,
    frames: Vec<Frame>,
    consts: Vec<Value>,
    natives: Vec<NativeFn>,
    proto_names: Vec<String>,
    pub out: Box<dyn Write>,
    pub input: Box<dyn BufRead>,
    pub args: Vec<String>,
    pub(crate) started: Instant,
    pub(crate) rng: u64,
    pub max_frames: usize,
}

pub const DEFAULT_MAX_FRAMES: usize = 20_000;

impl Vm {
    /// Prepares a program to run. Fails if it uses a native the VM does
    /// not provide.
    pub fn new(program: Program) -> Result<Vm, String> {
        let mut natives_table = Vec::new();
        for name in &program.natives {
            match natives::lookup(name) {
                Some(f) => natives_table.push(f),
                None => return Err(format!("unknown native function `{name}`")),
            }
        }
        let mut heap = Heap::new();
        let consts = program
            .consts
            .iter()
            .map(|c| match c {
                Const::Int(i) => Value::Int(*i),
                Const::Float(f) => Value::Float(*f),
                Const::Str(s) => heap.new_str(s.clone()),
            })
            .collect();
        let proto_names = program.protos.iter().map(|p| p.name.clone()).collect();
        Ok(Vm {
            program: Rc::new(program),
            heap,
            stack: Vec::with_capacity(1024),
            frames: Vec::new(),
            consts,
            natives: natives_table,
            proto_names,
            out: Box::new(std::io::stdout()),
            input: Box::new(std::io::BufReader::new(std::io::stdin())),
            args: Vec::new(),
            started: Instant::now(),
            rng: 0x2545_F491_4F6C_DD1D,
            max_frames: DEFAULT_MAX_FRAMES,
        })
    }

    pub fn run_main(&mut self) -> Result<(), RuntimeError> {
        let main = self.program.main.expect("program has no main");
        self.call(main, &[])?;
        let _ = self.out.flush();
        Ok(())
    }

    /// Calls a compiled function with arguments and runs it to completion.
    pub fn call(&mut self, func: u32, args: &[Value]) -> Result<Value, RuntimeError> {
        let depth = self.frames.len();
        let base = self.stack.len().max(self.frames.last().map_or(0, |f| {
            f.base + self.program.protos[f.func as usize].num_regs as usize
        }));
        let ret = base;
        let need = base + self.program.protos[func as usize].num_regs.max(1) as usize + 1;
        if self.stack.len() < need {
            self.stack.resize(need, Value::Unit);
        }
        for (i, a) in args.iter().enumerate() {
            self.stack[base + i] = *a;
        }
        self.frames.push(Frame {
            func,
            ip: 0,
            base,
            ret,
            closure: None,
        });
        let result = self.execute(depth);
        if result.is_err() {
            self.frames.truncate(depth);
        }
        result
    }

    pub fn display(&self, v: Value) -> String {
        ops::display(&self.heap, &self.program.types, &self.proto_names, v)
    }

    pub fn debug(&self, v: Value) -> String {
        ops::debug(&self.heap, &self.program.types, &self.proto_names, v)
    }

    pub(crate) fn some(&mut self, v: Value) -> Value {
        let adt = self.program.option_adt;
        Value::Obj(self.heap.alloc(Object::Variant {
            adt,
            tag: 0,
            fields: vec![v].into_boxed_slice(),
        }))
    }

    pub(crate) fn none(&self) -> Value {
        Value::Tag {
            adt: self.program.option_adt,
            tag: 1,
        }
    }

    pub(crate) fn ok(&mut self, v: Value) -> Value {
        let adt = self.program.result_adt;
        Value::Obj(self.heap.alloc(Object::Variant {
            adt,
            tag: 0,
            fields: vec![v].into_boxed_slice(),
        }))
    }

    pub(crate) fn err(&mut self, v: Value) -> Value {
        let adt = self.program.result_adt;
        Value::Obj(self.heap.alloc(Object::Variant {
            adt,
            tag: 1,
            fields: vec![v].into_boxed_slice(),
        }))
    }

    fn trace(&self) -> Vec<TraceLine> {
        let mut out = Vec::new();
        for f in self.frames.iter().rev() {
            let p = &self.program.protos[f.func as usize];
            if p.builtin {
                continue;
            }
            let ip = f.ip.saturating_sub(1);
            out.push(TraceLine {
                function: p.name.clone(),
                file: p.file.clone(),
                line: p.lines.get(ip).copied().unwrap_or(0),
            });
        }
        out
    }

    fn error(&self, kind: ErrorKind) -> RuntimeError {
        RuntimeError {
            kind,
            trace: self.trace(),
        }
    }

    fn collect_garbage(&mut self, top: usize) {
        let roots = self.stack[..top.min(self.stack.len())]
            .iter()
            .copied()
            .chain(self.consts.iter().copied())
            .chain(
                self.frames
                    .iter()
                    .filter_map(|f| f.closure.map(Value::Obj)),
            )
            .collect::<Vec<_>>();
        self.heap.collect(roots.into_iter());
    }

    fn push_frame(&mut self, func: u32, base: usize, ret: usize, argc: usize, closure: Option<Gc>) -> Result<(), RuntimeError> {
        if self.frames.len() >= self.max_frames {
            return Err(self.error(ErrorKind::Panic("stack overflow".into())));
        }
        let nregs = self.program.protos[func as usize].num_regs as usize;
        let need = base + nregs + 1;
        if self.stack.len() < need {
            self.stack.resize(need, Value::Unit);
        }
        // Clear registers past the arguments so stale values are not
        // treated as roots.
        for slot in &mut self.stack[base + argc..base + nregs] {
            *slot = Value::Unit;
        }
        self.frames.push(Frame {
            func,
            ip: 0,
            base,
            ret,
            closure,
        });
        Ok(())
    }

    fn execute(&mut self, stop_depth: usize) -> Result<Value, RuntimeError> {
        let program = self.program.clone();
        let mut fi = self.frames.len() - 1;
        let mut code: &[Op] = &program.protos[self.frames[fi].func as usize].code;
        let mut ip = self.frames[fi].ip;
        let mut base = self.frames[fi].base;
        let mut top = base + program.protos[self.frames[fi].func as usize].num_regs as usize;

        macro_rules! reg {
            ($r:expr) => {
                self.stack[base + $r as usize]
            };
        }
        macro_rules! fail {
            ($msg:expr) => {{
                self.frames[fi].ip = ip;
                let e = self.error(ErrorKind::Panic($msg));
                return Err(e);
            }};
        }
        macro_rules! int_op {
            ($dst:expr, $a:expr, $b:expr, $method:ident, $sym:expr) => {{
                let (x, y) = (reg!($a).as_int(), reg!($b).as_int());
                match x.$method(y) {
                    Some(v) => reg!($dst) = Value::Int(v),
                    None => fail!(format!("integer overflow in `{}`", $sym)),
                }
            }};
        }
        macro_rules! float_op {
            ($dst:expr, $a:expr, $b:expr, $op:tt) => {{
                let v = reg!($a).as_float() $op reg!($b).as_float();
                reg!($dst) = Value::Float(v);
            }};
        }
        macro_rules! reload {
            () => {{
                fi = self.frames.len() - 1;
                let f = &self.frames[fi];
                let p = &program.protos[f.func as usize];
                code = &p.code;
                ip = f.ip;
                base = f.base;
                top = base + p.num_regs as usize;
            }};
        }
        macro_rules! safepoint {
            () => {
                if self.heap.should_collect() {
                    self.frames[fi].ip = ip;
                    self.collect_garbage(top);
                }
            };
        }
        macro_rules! jump {
            ($offset:expr) => {{
                let off = $offset;
                ip = (ip as i64 + off as i64) as usize;
                if off < 0 {
                    safepoint!();
                }
            }};
        }

        loop {
            let op = code[ip];
            ip += 1;
            match op {
                Op::LoadConst { dst, idx } => reg!(dst) = self.consts[idx as usize],
                Op::LoadInt { dst, value } => reg!(dst) = Value::Int(value as i64),
                Op::LoadBool { dst, value } => reg!(dst) = Value::Bool(value),
                Op::LoadUnit { dst } => reg!(dst) = Value::Unit,
                Op::LoadTag { dst, adt, tag } => reg!(dst) = Value::Tag { adt, tag },
                Op::Move { dst, src } => reg!(dst) = reg!(src),

                Op::AddI { dst, a, b } => int_op!(dst, a, b, checked_add, "+"),
                Op::SubI { dst, a, b } => int_op!(dst, a, b, checked_sub, "-"),
                Op::MulI { dst, a, b } => int_op!(dst, a, b, checked_mul, "*"),
                Op::DivI { dst, a, b } => {
                    if reg!(b).as_int() == 0 {
                        fail!("division by zero".into());
                    }
                    int_op!(dst, a, b, checked_div, "/")
                }
                Op::RemI { dst, a, b } => {
                    if reg!(b).as_int() == 0 {
                        fail!("remainder by zero".into());
                    }
                    int_op!(dst, a, b, checked_rem, "%")
                }
                Op::NegI { dst, src } => match reg!(src).as_int().checked_neg() {
                    Some(v) => reg!(dst) = Value::Int(v),
                    None => fail!("integer overflow in `-`".into()),
                },
                Op::AddImm { dst, a, value } => match reg!(a).as_int().checked_add(value as i64) {
                    Some(v) => reg!(dst) = Value::Int(v),
                    None => fail!("integer overflow in `+`".into()),
                },
                Op::AddF { dst, a, b } => float_op!(dst, a, b, +),
                Op::SubF { dst, a, b } => float_op!(dst, a, b, -),
                Op::MulF { dst, a, b } => float_op!(dst, a, b, *),
                Op::DivF { dst, a, b } => float_op!(dst, a, b, /),
                Op::RemF { dst, a, b } => float_op!(dst, a, b, %),
                Op::NegF { dst, src } => reg!(dst) = Value::Float(-reg!(src).as_float()),
                Op::AddG { dst, a, b }
                | Op::SubG { dst, a, b }
                | Op::MulG { dst, a, b }
                | Op::DivG { dst, a, b }
                | Op::RemG { dst, a, b } => {
                    let (x, y) = (reg!(a), reg!(b));
                    let r = match (x, y) {
                        (Value::Int(x), Value::Int(y)) => {
                            let (r, sym) = match op {
                                Op::AddG { .. } => (x.checked_add(y), "+"),
                                Op::SubG { .. } => (x.checked_sub(y), "-"),
                                Op::MulG { .. } => (x.checked_mul(y), "*"),
                                Op::DivG { .. } => {
                                    if y == 0 {
                                        fail!("division by zero".into());
                                    }
                                    (x.checked_div(y), "/")
                                }
                                _ => {
                                    if y == 0 {
                                        fail!("remainder by zero".into());
                                    }
                                    (x.checked_rem(y), "%")
                                }
                            };
                            match r {
                                Some(v) => Value::Int(v),
                                None => fail!(format!("integer overflow in `{sym}`")),
                            }
                        }
                        (Value::Float(x), Value::Float(y)) => Value::Float(match op {
                            Op::AddG { .. } => x + y,
                            Op::SubG { .. } => x - y,
                            Op::MulG { .. } => x * y,
                            Op::DivG { .. } => x / y,
                            _ => x % y,
                        }),
                        _ if matches!(op, Op::AddG { .. }) => {
                            let s = format!("{}{}", self.heap.str(x), self.heap.str(y));
                            self.heap.new_str(s)
                        }
                        _ => fail!("arithmetic on non-numbers".into()),
                    };
                    reg!(dst) = r;
                }
                Op::NegG { dst, src } => {
                    reg!(dst) = match reg!(src) {
                        Value::Int(i) => match i.checked_neg() {
                            Some(v) => Value::Int(v),
                            None => fail!("integer overflow in `-`".into()),
                        },
                        Value::Float(f) => Value::Float(-f),
                        _ => fail!("negation of a non-number".into()),
                    }
                }
                Op::Concat { dst, a, b } => {
                    let (x, y) = (reg!(a), reg!(b));
                    let xs = self.heap.str(x);
                    let ys = self.heap.str(y);
                    let mut s = String::with_capacity(xs.len() + ys.len());
                    s.push_str(xs);
                    s.push_str(ys);
                    reg!(dst) = self.heap.new_str(s);
                }
                Op::BitAnd { dst, a, b } => reg!(dst) = Value::Int(reg!(a).as_int() & reg!(b).as_int()),
                Op::BitOr { dst, a, b } => reg!(dst) = Value::Int(reg!(a).as_int() | reg!(b).as_int()),
                Op::BitXor { dst, a, b } => reg!(dst) = Value::Int(reg!(a).as_int() ^ reg!(b).as_int()),
                Op::Shl { dst, a, b } | Op::Shr { dst, a, b } => {
                    let n = reg!(b).as_int();
                    if !(0..64).contains(&n) {
                        fail!(format!("shift by {n} is out of range (0 to 63)"));
                    }
                    let x = reg!(a).as_int();
                    reg!(dst) = Value::Int(if matches!(op, Op::Shl { .. }) { x << n } else { x >> n });
                }
                Op::Not { dst, src } => reg!(dst) = Value::Bool(!reg!(src).as_bool()),

                Op::Eq { dst, a, b } => reg!(dst) = Value::Bool(ops::equal(&self.heap, reg!(a), reg!(b))),
                Op::Ne { dst, a, b } => reg!(dst) = Value::Bool(!ops::equal(&self.heap, reg!(a), reg!(b))),
                Op::LtI { dst, a, b } => reg!(dst) = Value::Bool(reg!(a).as_int() < reg!(b).as_int()),
                Op::LeI { dst, a, b } => reg!(dst) = Value::Bool(reg!(a).as_int() <= reg!(b).as_int()),
                Op::LtF { dst, a, b } => reg!(dst) = Value::Bool(reg!(a).as_float() < reg!(b).as_float()),
                Op::LeF { dst, a, b } => reg!(dst) = Value::Bool(reg!(a).as_float() <= reg!(b).as_float()),
                Op::Lt { dst, a, b } => {
                    let (x, y) = (reg!(a), reg!(b));
                    let lt = match (x, y) {
                        (Value::Float(p), Value::Float(q)) => p < q,
                        _ => ops::compare(&self.heap, x, y) == Ordering::Less,
                    };
                    reg!(dst) = Value::Bool(lt);
                }
                Op::Le { dst, a, b } => {
                    let (x, y) = (reg!(a), reg!(b));
                    let le = match (x, y) {
                        (Value::Float(p), Value::Float(q)) => p <= q,
                        _ => ops::compare(&self.heap, x, y) != Ordering::Greater,
                    };
                    reg!(dst) = Value::Bool(le);
                }

                Op::Jump { offset } => jump!(offset),
                Op::JumpIf { cond, offset } => {
                    if reg!(cond).as_bool() {
                        jump!(offset);
                    }
                }
                Op::JumpIfNot { cond, offset } => {
                    if !reg!(cond).as_bool() {
                        jump!(offset);
                    }
                }
                Op::JumpIfNotLtI { a, b, offset } => {
                    if reg!(a).as_int() >= reg!(b).as_int() {
                        jump!(offset);
                    }
                }

                Op::Call { base: b, argc } => {
                    safepoint!();
                    let callee = reg!(b);
                    let g = callee.as_obj();
                    let func = match self.heap.get(g) {
                        Object::Closure { func, .. } => *func,
                        _ => fail!("called a value that is not a function".into()),
                    };
                    self.frames[fi].ip = ip;
                    let abs = base + b as usize;
                    self.push_frame(func, abs + 1, abs, argc as usize, Some(g))?;
                    reload!();
                }
                Op::CallDirect { func, base: b, argc } => {
                    safepoint!();
                    self.frames[fi].ip = ip;
                    let abs = base + b as usize;
                    self.push_frame(func, abs, abs, argc as usize, None)?;
                    reload!();
                }
                Op::CallNative { native, base: b, argc } => {
                    self.frames[fi].ip = ip;
                    let abs = base + b as usize;
                    let mut args = [Value::Unit; 8];
                    let n = argc as usize;
                    args[..n].copy_from_slice(&self.stack[abs..abs + n]);
                    let f = self.natives[native as usize];
                    match f(self, &args[..n]) {
                        Ok(v) => self.stack[abs] = v,
                        Err(NativeError::Panic(msg)) => return Err(self.error(ErrorKind::Panic(msg))),
                        Err(NativeError::Exit(code)) => return Err(self.error(ErrorKind::Exit(code))),
                    }
                }
                Op::Return { src } => {
                    let v = reg!(src);
                    let f = self.frames.pop().unwrap();
                    self.stack[f.ret] = v;
                    if self.frames.len() == stop_depth {
                        return Ok(v);
                    }
                    reload!();
                }
                Op::ReturnUnit => {
                    let f = self.frames.pop().unwrap();
                    self.stack[f.ret] = Value::Unit;
                    if self.frames.len() == stop_depth {
                        return Ok(Value::Unit);
                    }
                    reload!();
                }

                Op::MakeClosure { dst, func, base: b, count } => {
                    let start = base + b as usize;
                    let captures: Box<[Value]> = self.stack[start..start + count as usize].into();
                    let g = self.heap.alloc(Object::Closure { func, captures });
                    reg!(dst) = Value::Obj(g);
                }
                Op::GetCapture { dst, idx } => {
                    let g = self.frames[fi].closure.expect("capture outside a closure");
                    reg!(dst) = match self.heap.get(g) {
                        Object::Closure { captures, .. } => captures[idx as usize],
                        _ => unreachable!(),
                    };
                }
                Op::NewCell { dst, src } => {
                    let v = reg!(src);
                    reg!(dst) = Value::Obj(self.heap.alloc(Object::Cell(v)));
                }
                Op::CellGet { dst, cell } => {
                    reg!(dst) = match self.heap.get(reg!(cell).as_obj()) {
                        Object::Cell(v) => *v,
                        _ => unreachable!(),
                    };
                }
                Op::CellSet { cell, src } => {
                    let v = reg!(src);
                    if let Object::Cell(slot) = self.heap.get_mut(reg!(cell).as_obj()) {
                        *slot = v;
                    }
                }

                Op::NewStruct { dst, adt, base: b, count } => {
                    let start = base + b as usize;
                    let fields: Box<[Value]> = self.stack[start..start + count as usize].into();
                    reg!(dst) = Value::Obj(self.heap.alloc(Object::Struct { adt, fields }));
                }
                Op::NewVariant { dst, adt, tag, base: b, count } => {
                    let start = base + b as usize;
                    let fields: Box<[Value]> = self.stack[start..start + count as usize].into();
                    reg!(dst) = Value::Obj(self.heap.alloc(Object::Variant { adt, tag, fields }));
                }
                Op::NewTuple { dst, base: b, count } => {
                    let start = base + b as usize;
                    let fields: Box<[Value]> = self.stack[start..start + count as usize].into();
                    reg!(dst) = Value::Obj(self.heap.alloc(Object::Tuple(fields)));
                }
                Op::NewArray { dst, base: b, count } => {
                    let start = base + b as usize;
                    let items = self.stack[start..start + count as usize].to_vec();
                    reg!(dst) = self.heap.new_array(items);
                }
                Op::NewRange { dst, lo, hi } => {
                    let (l, h) = (reg!(lo).as_int(), reg!(hi).as_int());
                    reg!(dst) = Value::Obj(self.heap.alloc(Object::Range(l, h)));
                }
                Op::GetField { dst, obj, idx } => {
                    reg!(dst) = match self.heap.get(reg!(obj).as_obj()) {
                        Object::Struct { fields, .. } | Object::Variant { fields, .. } | Object::Tuple(fields) => {
                            fields[idx as usize]
                        }
                        Object::Range(lo, hi) => Value::Int(if idx == 0 { *lo } else { *hi }),
                        other => panic!("GetField on {other:?}"),
                    };
                }
                Op::SetField { obj, idx, src } => {
                    let v = reg!(src);
                    match self.heap.get_mut(reg!(obj).as_obj()) {
                        Object::Struct { fields, .. } => fields[idx as usize] = v,
                        other => panic!("SetField on {other:?}"),
                    }
                }
                Op::JumpIfNotTag { obj, tag, offset } => {
                    let t = match reg!(obj) {
                        Value::Tag { tag, .. } => tag,
                        Value::Obj(g) => match self.heap.get(g) {
                            Object::Variant { tag, .. } => *tag,
                            _ => u32::MAX,
                        },
                        _ => u32::MAX,
                    };
                    if t != tag {
                        jump!(offset);
                    }
                }

                Op::Index { dst, obj, idx } => {
                    let o = reg!(obj);
                    let i = reg!(idx);
                    let v = match self.heap.get(o.as_obj()) {
                        Object::Array(items) => {
                            let n = i.as_int();
                            match usize::try_from(n).ok().and_then(|n| items.get(n)) {
                                Some(v) => *v,
                                None => fail!(format!("index {n} is out of bounds for an array of length {}", items.len())),
                            }
                        }
                        Object::Map(m) => {
                            let key = ops::key_of(&self.heap, i).expect("map key");
                            match m.get(&key) {
                                Some(v) => v,
                                None => {
                                    let k = self.debug(i);
                                    fail!(format!("key {k} is not in the map"))
                                }
                            }
                        }
                        other => panic!("Index on {other:?}"),
                    };
                    reg!(dst) = v;
                }
                Op::SetIndex { obj, idx, src } => {
                    let o = reg!(obj);
                    let i = reg!(idx);
                    let v = reg!(src);
                    let key = match self.heap.get(o.as_obj()) {
                        Object::Map(_) => Some(ops::key_of(&self.heap, i).expect("map key")),
                        _ => None,
                    };
                    match self.heap.get_mut(o.as_obj()) {
                        Object::Array(items) => {
                            let n = i.as_int();
                            let len = items.len();
                            match usize::try_from(n).ok().and_then(|n| items.get_mut(n)) {
                                Some(slot) => *slot = v,
                                None => fail!(format!("index {n} is out of bounds for an array of length {len}")),
                            }
                        }
                        Object::Map(m) => m.insert(key.unwrap(), i, v),
                        other => panic!("SetIndex on {other:?}"),
                    }
                }
                Op::Len { dst, obj } => {
                    let n = self.heap.array(reg!(obj)).len();
                    reg!(dst) = Value::Int(n as i64);
                }
                Op::ToStr { dst, src } => {
                    let v = reg!(src);
                    let is_str = matches!(v, Value::Obj(g) if matches!(self.heap.get(g), Object::Str(_)));
                    reg!(dst) = if is_str {
                        v
                    } else {
                        let s = self.display(v);
                        self.heap.new_str(s)
                    };
                }
                Op::Panic { msg } => {
                    let m = match &program.consts[msg as usize] {
                        Const::Str(s) => s.clone(),
                        other => format!("{other:?}"),
                    };
                    fail!(m);
                }
            }
        }
    }
}
