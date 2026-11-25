//! The instruction set and compiled program format.
//!
//! The VM is register based: each call frame has a window of registers on a
//! shared stack, and instructions name registers directly. Arguments are
//! passed by placing them in consecutive registers, which become the first
//! registers of the callee's window, so calls copy nothing.
//!
//! Instructions are typed where the checker knows the types (`AddI` for
//! Int, `AddF` for Float), so the common cases do not inspect value tags.
//! The generic forms (`AddG`, `Lt`) are used inside generic functions.

pub type Reg = u16;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Op {
    LoadConst {
        dst: Reg,
        idx: u32,
    },
    LoadInt {
        dst: Reg,
        value: i32,
    },
    LoadBool {
        dst: Reg,
        value: bool,
    },
    LoadUnit {
        dst: Reg,
    },
    /// A variant without fields.
    LoadTag {
        dst: Reg,
        adt: u32,
        tag: u32,
    },
    Move {
        dst: Reg,
        src: Reg,
    },

    AddI {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    SubI {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    MulI {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    DivI {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    RemI {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    NegI {
        dst: Reg,
        src: Reg,
    },
    /// `dst = a + value`, for loop counters.
    AddImm {
        dst: Reg,
        a: Reg,
        value: i32,
    },
    AddF {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    SubF {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    MulF {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    DivF {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    RemF {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    NegF {
        dst: Reg,
        src: Reg,
    },
    AddG {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    SubG {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    MulG {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    DivG {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    RemG {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    NegG {
        dst: Reg,
        src: Reg,
    },
    Concat {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    BitAnd {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    BitOr {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    BitXor {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    Shl {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    Shr {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    Not {
        dst: Reg,
        src: Reg,
    },

    Eq {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    Ne {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    LtI {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    LeI {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    LtF {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    LeF {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    /// Generic ordering (strings, tuples, arrays, type parameters).
    Lt {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    Le {
        dst: Reg,
        a: Reg,
        b: Reg,
    },

    Jump {
        offset: i32,
    },
    JumpIf {
        cond: Reg,
        offset: i32,
    },
    JumpIfNot {
        cond: Reg,
        offset: i32,
    },
    /// Jumps unless `a < b` (Ints); the loop test of `for i in a..b`.
    JumpIfNotLtI {
        a: Reg,
        b: Reg,
        offset: i32,
    },
    /// Jumps unless `a <= b` (Ints).
    JumpIfNotLeI {
        a: Reg,
        b: Reg,
        offset: i32,
    },

    /// Calls the closure in `base` with `argc` arguments after it; the
    /// result replaces the closure.
    Call {
        base: Reg,
        argc: u8,
    },
    /// Calls a known function with arguments starting at `base`; the result
    /// goes in `base`.
    CallDirect {
        func: u32,
        base: Reg,
        argc: u8,
    },
    CallNative {
        native: u32,
        base: Reg,
        argc: u8,
    },
    Return {
        src: Reg,
    },
    ReturnUnit,

    /// Builds a closure over the values in `count` registers from `base`.
    MakeClosure {
        dst: Reg,
        func: u32,
        base: Reg,
        count: u16,
    },
    GetCapture {
        dst: Reg,
        idx: u16,
    },
    NewCell {
        dst: Reg,
        src: Reg,
    },
    CellGet {
        dst: Reg,
        cell: Reg,
    },
    CellSet {
        cell: Reg,
        src: Reg,
    },

    NewStruct {
        dst: Reg,
        adt: u32,
        base: Reg,
        count: u16,
    },
    NewVariant {
        dst: Reg,
        adt: u32,
        tag: u32,
        base: Reg,
        count: u16,
    },
    NewTuple {
        dst: Reg,
        base: Reg,
        count: u16,
    },
    NewArray {
        dst: Reg,
        base: Reg,
        count: u16,
    },
    NewRange {
        dst: Reg,
        lo: Reg,
        hi: Reg,
    },
    /// Field of a struct, tuple, variant payload or range.
    GetField {
        dst: Reg,
        obj: Reg,
        idx: u16,
    },
    SetField {
        obj: Reg,
        idx: u16,
        src: Reg,
    },
    /// Jumps unless the enum value in `obj` has this tag.
    JumpIfNotTag {
        obj: Reg,
        tag: u32,
        offset: i32,
    },

    /// `arr[idx]` or `map[key]`.
    Index {
        dst: Reg,
        obj: Reg,
        idx: Reg,
    },
    SetIndex {
        obj: Reg,
        idx: Reg,
        src: Reg,
    },
    Len {
        dst: Reg,
        obj: Reg,
    },
    /// Converts any value to its display string.
    ToStr {
        dst: Reg,
        src: Reg,
    },
    /// Panics with the string constant.
    Panic {
        msg: u32,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Const {
    Int(i64),
    Float(f64),
    Str(String),
}

#[derive(Clone, Debug, Default)]
pub struct Proto {
    pub name: String,
    pub file: String,
    pub arity: u16,
    pub num_regs: u16,
    pub code: Vec<Op>,
    /// Source line of each instruction.
    pub lines: Vec<u32>,
    /// From the prelude; hidden in stack traces.
    pub builtin: bool,
}

/// Names needed to print values.
#[derive(Clone, Debug, Default)]
pub struct TypeInfo {
    pub name: String,
    pub fields: Vec<String>,
    pub variants: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Program {
    pub protos: Vec<Proto>,
    pub consts: Vec<Const>,
    pub natives: Vec<String>,
    pub types: Vec<TypeInfo>,
    pub main: Option<u32>,
    /// (name, proto) for each `test` block.
    pub tests: Vec<(String, u32)>,
    pub option_adt: u32,
    pub result_adt: u32,
}
