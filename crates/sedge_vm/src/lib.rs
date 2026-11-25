//! The Sedge virtual machine: bytecode, heap, garbage collector and
//! interpreter.

pub mod bytecode;
pub mod disasm;
pub mod map;
mod natives;
pub mod ops;
pub mod value;
pub mod vm;

pub use bytecode::{Const, Op, Program, Proto, Reg, TypeInfo};
pub use value::{Heap, Object, Value};
pub use vm::{ErrorKind, RuntimeError, TraceLine, Vm};

/// Whether the VM provides a native function with this name.
pub fn has_native(name: &str) -> bool {
    natives::lookup(name).is_some()
}
