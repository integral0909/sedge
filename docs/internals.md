# Sedge internals

This document follows a program through the toolchain, from source text
to a running VM, and explains the main design decisions along the way.

## 1. Source and diagnostics (`sedge_syntax::source`, `diag`)

Every file goes into a `SourceMap`, which gives it its own range of
global byte offsets. A `Span` is therefore just `(lo, hi)`: 8 bytes, with
no file id, yet it can always be traced back to a file, line and column.
The prelude is file 0, and user files follow it.

Diagnostics carry a primary span with a label, secondary labels and
notes. They are rendered rustc-style with source excerpts. The language
server turns the same values into LSP diagnostics with UTF-16 columns.

## 2. Lexing and semicolon insertion (`lexer.rs`)

The lexer inserts a `;` at a line break when both of these hold:

1. The previous token can end a statement: an identifier, a literal,
   `)`, `]`, `}`, `?`, `return`, `break` or `continue`.
2. The innermost open bracket is not `(` or `[`.

A line that starts with `.` cancels the insertion, so method chains can
span lines. A line break before a closing bracket is judged in the
context that the bracket closes. For example, the `x` in
`f(fn(x) {\n x\n})` gets its semicolon because it is inside braces, even
though those braces are inside parentheses.

String interpolation is handled in the lexer. It finds the matching `}`
of each `${` (keeping track of nested braces and strings) and records the
span of the expression inside. The parser then lexes and parses that span
as an expression, using the same global offsets, so errors inside
interpolations point at the right place.

## 3. Parsing (`parser.rs`)

The parser is recursive descent with precedence climbing for binary
operators. Expressions and patterns get a `NodeId`. Later passes keep
their results in side tables keyed by id instead of changing the tree.
Two context rules come from Rust:

- Struct literals are not allowed in the condition of `if`, `while`,
  `for` and `match`, where `{` starts the body. Write `match (P { .. })`.
- `return`, `break` and `continue` can appear where an expression is
  expected, as in `None => break`. They are parsed as a block holding the
  statement, which the checker gives the type `Never`.

Error recovery skips to the end of the statement (or item) and carries
on, so one mistake does not hide the rest. Every loop in the parser
checks that it consumed at least one token per iteration. The mutation
test in `crates/sedge/tests/robustness.rs` was added to find exactly the
cases where that did not hold.

## 4. Checking (`sedge_check`)

Checking runs in four passes over all modules: type names; struct fields
and enum variants; function signatures (including methods in `impl`
blocks); then bodies. Thanks to the early passes, functions and types can
be used before they are declared, and they can be mutually recursive.

### Inference

Types are inferred by unification. `Ty::Var` is an inference variable,
and the `Unifier` keeps a solution vector with an occurs check. Every use
of a generic function, method or variant constructor is *instantiated*:
each type parameter is replaced by a fresh variable. Unification then
works the actual types out from the arguments:

```sedge
fn main() {
    let names = Map.new()      // Map[?K, ?V]
    names.set("a", 1.5)        // ?K = String, ?V = Float
    let doubled = [1, 2].map(fn(x) => x * 2)   // [Int]
    print(names)
    print(doubled)
}
```

Checking is bidirectional where that helps: an expected type flows into
lambdas (to give their parameters types), array literals and the
branches of `if` and `match`. That is why
`xs.map(fn(x) => x.to_float())` needs no annotation. By the time the
lambda is checked, `xs` has already fixed the parameter type.

Type parameters inside a generic body are rigid (`Ty::Param`), and they
only unify with themselves.

### Obligations

Some constraints cannot be decided at the point where they arise. In
`fn(a, b) => a + b`, whether `+` means Int addition, Float addition or
string concatenation depends on how the lambda is used later. The
checker records an obligation (`Addable`, `Num`, `Ord` or `Key`) and
solves all of them once every body has been checked. Unconstrained
numeric variables default to `Int`. A failed obligation becomes an error
that names the operator and, for type parameters, the bound to add
(`declare the bound on the type parameter: T: Ord`).

### Closures

Each function or lambda body is a *context*. When a name resolves to a
local from an outer context, the local is marked `captured` and added to
the capture list of every context in between. A captured `var` is also
marked `boxed`. The compiler stores boxed locals in heap cells, so the
function and all its closures share one mutable slot. Captured `let`
bindings cannot change, so they are copied into the closure.

### Exhaustiveness

`exhaust.rs` implements Maranget's usefulness algorithm ("Warnings for
pattern matching", 2007). Patterns are lowered to constructors and
wildcards. A `match` is exhaustive if a row of wildcards is *not useful*
with respect to its arms. An arm is unreachable if it is not useful with
respect to the arms above it. When the wildcard row is useful, the
recursion builds a witness value, which the error message prints
(`` `Some(false)` is not covered ``). Arms with guards count for
reachability but not for coverage. `let`, `for` and parameters run the
same check to reject refutable patterns.

### Output

`Typed` holds the declarations (ADTs, functions, locals, type parameters)
and the side tables: the type of every expression, what each name
resolves to, how each call is made (direct, static, variant constructor,
closure), the field index of each field access, and what each pattern
node does. The language server uses one more table, the list of every
name use with its definition and type.

## 5. Compiling (`sedge_compile`)

The compiler walks the checked tree and emits register bytecode.

**Registers** are allocated like a stack. A local takes the next free
register for the rest of its scope, and temporaries above it are freed at
the end of each statement. `expr(e, dst)` compiles `e` into `dst` if one
is given. Otherwise it may return any register holding the value,
including a local's own register, which avoids moves.

**Calls** put their arguments in the registers at the top of the frame.
Those registers become the first registers of the callee's frame, so
nothing is copied. A direct call writes its result back to the base
register.

**Typed instructions.** The checker knows the operand types, so `a + b`
becomes `AddI`, `AddF` or `Concat`. Only generic code bounded by `Num`
uses `AddG`, which dispatches at run time. Comparisons work the same way.

**Peephole forms.** These cover the common patterns:

- `x + k` and `x - k` with a small constant become `AddImm`.
- `if a < b` on Ints becomes one `JumpIfNotLtI` instead of a compare
  followed by a branch.
- `for i in a..b` uses a counter register and the same fused jump.
- An `if`/`else` in return position emits `return` in each branch.

The result for `fib`:

```
fn #49 fib (arity 1, 5 registers)
   3     0  r1 = 2
   |     1  unless r0 < r1 goto 3
   |     2  return r0
   |     3  r2 = r0 + -1
   |     4  r2 = fib(r2..+1)
   |     5  r3 = r0 + -2
   |     6  r3 = fib(r3..+1)
   |     7  addi r4 r2 r3
   |     8  return r4
```

**Pattern matching** compiles to a sequence of tests per arm. Each failed
test jumps to the next arm. Tests read the variant tag
(`JumpIfNotTag`), compare literals, and extract fields with `GetField`
into fresh registers before testing nested patterns. Alternatives in an
or-pattern bind the same locals as the first alternative.

**Functions as values.** A named function used as a value becomes a
closure over no captures. Natives and variant constructors used as
values (`xs.map(str)`, `xs.map(Some)`) get small generated wrapper
functions.

## 6. The VM (`sedge_vm`)

### Values

`Value` is a copyable enum: `Unit`, `Bool`, `Int`, `Float`,
`Tag { adt, tag }` (a variant without fields, which needs no heap
allocation) and `Obj(Gc)`. A `Gc` is an index into the heap's slot
vector. The VM contains no `unsafe` code.

### Heap and garbage collection

Objects are strings, arrays, insertion-ordered maps, structs, variants,
tuples, closures, cells and ranges. Collection is mark and sweep with
precise roots: the register stack up to the top of the current frame,
the constant pool and the closures of live frames. It runs only at
**safepoints** (calls and backward jumps). At a safepoint every live
value is in a register, so natives and instructions that allocate
several objects in sequence never have to protect their temporaries.
When a frame is pushed, the registers beyond its arguments are cleared,
so stale values cannot act as roots. The next collection threshold is
twice the live size after each collection.

### Frames and calls

A frame records its function, instruction pointer, register base, the
slot that receives its return value, and its closure (if it has one).
The interpreter keeps `code`, `ip` and `base` in locals and reloads them
only on call and return. Natives get their arguments copied into a small
array on the Rust stack, and they return `Result<Value, NativeError>`.

### Errors

Panics carry a stack trace built from each frame's line table. Prelude
frames are hidden, so a failed `unwrap` points at the caller's line.
`exit(code)` uses the same path with a separate kind.

## 7. Language server (`sedge_lsp`)

The server re-analyzes the whole document on every change. That takes
milliseconds for files of a few thousand lines. Hover and
go-to-definition look up the innermost name use under the cursor in the
checker's use table. Completion after `.` uses the last version of the
document that checked cleanly, because the text being typed (`p.`)
almost never parses. It finds the closest earlier use of the receiver's
name to get its type, then lists that type's fields and methods.

## 8. Testing strategy

- **Unit tests** in each crate: the lexer's semicolon rules, parser
  recovery, unification, exhaustiveness witnesses, GC reachability, map
  ordering, JSON and the LSP request handlers.
- **Golden programs** in `tests/run`, `tests/errors` and `tests/panics`.
  Each states its expected stdout (`//> ...`) or the error fragments it
  must produce (`//! ...`).
- **Documentation examples:** every complete program in the docs is
  compiled and run.
- **Mutation testing:** every program is damaged at random (deleted
  ranges, inserted tokens, duplicated and swapped text), and analysis must
  finish without panicking within ten seconds. Its first run found a
  parser loop and a crash in the exhaustiveness checker.
