# Sedge

[![CI](https://github.com/integral0909/sedge/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/integral0909/sedge/actions/workflows/ci.yml)

Sedge is a small, statically typed scripting language. It has type
inference, generics, algebraic data types with exhaustive pattern matching,
and closures. It compiles to bytecode for a register-based virtual machine
with a garbage collector. The whole toolchain is one Rust binary with no
dependencies: compiler, VM, REPL, test runner, disassembler and a language
server for editors.

```sedge
enum Shape {
    Circle(Float),
    Rect(Float, Float),
}

fn area(s: Shape) -> Float {
    match s {
        Circle(r) => 3.14159 * r * r,
        Rect(w, h) => w * h,
    }
}

fn main() {
    let shapes = [Circle(1.0), Rect(2.0, 3.5)]
    let total = shapes.map(area).fold(0.0, fn(a, b) => a + b)
    print("total area: ${total.fixed(2)}")
}
```

Types are inferred inside functions and declared at function boundaries.
Mistakes are reported before anything runs. Leave out the `Rect` arm and
you get:

```
error: this `match` does not handle every case
 --> shapes.sg:7:11
  |
7 |     match s {
  |           ^ `Rect(_, _)` is not covered
```

## Install

```sh
cargo install --path crates/sedge
```

That needs Rust 1.82 or newer. The build has no external crates.

## Usage

```sh
sedge run examples/json.sg       # check, compile and run
sedge check program.sg           # report errors and warnings only
sedge test program.sg            # run the `test "..." { }` blocks
sedge disasm bench/fib.sg        # show the bytecode
sedge repl                       # interactive prompt (also plain `sedge`)
sedge lsp                        # language server on stdio
```

The REPL keeps your definitions and variables, and prints each expression's
value and type:

```
> fn sq(x: Int) -> Int { x * x }
> let xs = [1, 2, 3].map(sq)
> xs
[1, 4, 9] : [Int]
> :type xs.map(str)
[String]
```

## The language in brief

- **Types:** `Int` (64-bit, overflow is an error), `Float`, `Bool`, `String`,
  arrays `[T]`, `Map[K, V]` (insertion-ordered), tuples `(A, B)`, function
  types `fn(A) -> B`, and user-defined `struct` and `enum` types, all of
  which can be generic.
- **Inference:** unification-based, Hindley–Milner style within function
  bodies. Generic functions are instantiated at every use, so
  `[1, 2].map(fn(x) => x * 2)` needs no annotations.
- **Bounds:** type parameters can require `Ord` (comparison), `Num`
  (arithmetic) or `Key` (map keys): `fn max[T: Ord](xs: [T]) -> T`.
- **Pattern matching:** on variants, literals, tuples and structs, with
  `|` alternatives, `if` guards and nested patterns. Non-exhaustive matches
  are compile errors that name a missing case, and unreachable arms are
  warnings.
- **Errors as values:** `Option` and `Result` come from the prelude, and
  `?` returns early with `None` or `Err`.
- **Closures:** they capture `let` bindings by value and `var` bindings by
  reference, so counters and accumulators work as you would expect.
- **Methods:** `impl` blocks, with `self` methods and static functions such
  as `Point.new(1.0, 2.0)`.
- **Strings:** UTF-8, with interpolation (`"${x + 1}"`).
- **Statements end at line breaks.** Semicolons are optional, as in Go, and
  a line that starts with `.` continues a method chain.

The [language guide](docs/language.md) covers it all with examples. The
prelude is written mostly in Sedge itself:
[`crates/sedge_check/src/prelude.sg`](crates/sedge_check/src/prelude.sg).

## How it works

```
source ──▶ lexer ──▶ parser ──▶ checker ──▶ compiler ──▶ bytecode ──▶ VM
           (ASI)    (recovers    (inference,   (registers,              (GC,
                     from errors) exhaustiveness) closures)              natives)
                                     │
                                     └──▶ language server (hover, go to definition, ...)
```

| Crate | What it does |
|---|---|
| `sedge_syntax` | Source map, diagnostics, lexer with semicolon insertion, recursive-descent parser with error recovery |
| `sedge_check` | Name resolution, type inference, bounds as deferred obligations, closure capture analysis, Maranget's algorithm for exhaustiveness, the prelude |
| `sedge_compile` | Lowers checked trees to register bytecode: typed arithmetic, fused compare-and-branch, tail returns, decision code for patterns |
| `sedge_vm` | Instruction set, mark-and-sweep heap, interpreter, native functions, disassembler |
| `sedge_lsp` | Language Server Protocol over stdio, with its own JSON parser |
| `sedge` | The command-line tool and REPL |

[docs/internals.md](docs/internals.md) explains the design: how types are
inferred, how closures share variables, how the VM calls functions without
copying arguments, and when the collector runs.

## Performance

On an Apple M-series laptop (release build):

| Benchmark | Sedge | Python 3.14 | Lua 5.4 |
|---|---|---|---|
| `fib(32)`, recursive | 0.19 s | 0.33 s | 0.14 s |
| n-body, 1M steps (`examples/nbody.sg`) | 2.9 s | | |
| ray tracer, 256×256 (`examples/raytracer.sg`) | 0.5 s | | |

The instructions are typed. `a + b` on Ints compiles to `AddI`, and
`if a < b` becomes one compare-and-jump instruction. Calls pass arguments
in place on the register stack. There is no JIT.

## Editor support

`sedge lsp` works with any editor that speaks LSP. It provides diagnostics
as you type, hover with types and doc comments, go to definition, find
references, document symbols, and completion of fields and methods. For
example, in Neovim:

```lua
vim.lsp.start({ name = "sedge", cmd = { "sedge", "lsp" }, root_dir = vim.fn.getcwd() })
```

[`editors/`](editors/) has a TextMate grammar for syntax highlighting and
a minimal VS Code extension.

## Tests

```sh
cargo test --workspace
```

The tests are:

- Unit tests in every crate.
- Golden programs in [`tests/`](tests/) that state their expected output
  (`//> ...`), compile errors or runtime panics (`//! ...`).
- Language server sessions.
- A mutation test that damages every program in the repository thousands
  of ways and requires the front end to report errors without crashing or
  hanging. Raise the count with `SEDGE_MUTATIONS=2000`.

## Status and roadmap

Sedge is young. Things that are missing, roughly in the order they are
planned:

1. **Modules:** `import` across files with visibility. The checker already
   handles multiple files in one program.
2. **Traits:** user-defined interfaces in place of the fixed
   `Ord`/`Num`/`Key` bounds, with dictionary passing in the compiler.
3. **A formatter** that keeps comments (`sedge fmt`).
4. **Faster execution:** a generational collector, inline caches for field
   access, and NaN-boxed values.
5. **A debugger:** breakpoints and stepping through the Debug Adapter
   Protocol, using the line tables the compiler already emits.

## License

MIT
