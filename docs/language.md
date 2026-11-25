# The Sedge language

This guide covers the whole language. Every complete program in it (each
code block with a `main`) is compiled by the test suite, so the examples
stay correct.

- [Programs and statements](#programs-and-statements)
- [Values and types](#values-and-types)
- [Variables](#variables)
- [Functions and closures](#functions-and-closures)
- [Control flow](#control-flow)
- [Structs and methods](#structs-and-methods)
- [Enums and pattern matching](#enums-and-pattern-matching)
- [Generics and bounds](#generics-and-bounds)
- [Option, Result and `?`](#option-result-and-)
- [Arrays and maps](#arrays-and-maps)
- [Strings](#strings)
- [Tests](#tests)
- [Runtime errors](#runtime-errors)
- [The prelude](#the-prelude)

## Programs and statements

A program is a list of items: functions, structs, enums, `impl` blocks
and tests. Execution starts at `fn main()`.

```sedge
fn main() {
    print("hello")
}
```

A line break ends a statement if the line could end there, as in Go, so
semicolons are only needed to put several statements on one line. A line
break inside `( )` or `[ ]` never ends a statement. A line that starts
with `.` continues the previous one:

```sedge
fn main() {
    let total = [1, 2, 3, 4]
        .filter(fn(x) => x % 2 == 0)
        .fold(0, fn(a, b) => a + b)
    let pair = (1,
                2); print(total); print(pair)
}
```

Comments are `// line`, `/* block */` (these nest) and `/// doc`, which
documents the item below it and shows up in editor hovers.

## Values and types

| Type | Examples | Notes |
|---|---|---|
| `Int` | `42`, `-7`, `0xff`, `0b1010`, `1_000_000` | 64-bit. Overflow is a runtime error. |
| `Float` | `3.14`, `1e-9`, `2.0` | 64-bit IEEE. There is no implicit conversion: `n.to_float()`, `f.to_int()`. |
| `Bool` | `true`, `false` | |
| `String` | `"text"`, `"tab\tnew\nline"`, `"${x}"` | UTF-8, immutable. |
| `()` | `()` | The unit type, for "no value". |
| `[T]` | `[1, 2, 3]`, `[]` | Growable arrays. |
| `Map[K, V]` | `Map.new()` | Hash maps that keep insertion order. Keys are `Int`, `String` or `Bool`. |
| `(A, B)` | `(1, "one")` | Tuples. Fields are `.0`, `.1` and so on. |
| `fn(A) -> B` | `fn(x) => x + 1` | Functions and closures. |
| `Range` | `0..10`, `1..=5` | Used with `for`. |

Arithmetic needs both operands to have the same type. `+` also joins
strings. Comparisons work on numbers and strings, and on tuples and
arrays of them (lexicographically). `==` compares any two values of the
same type structurally.

## Variables

`let` binds a name once. `var` can be reassigned:

```sedge
fn main() {
    let name = "Sedge"
    var count = 0
    count += 1
    count = count * 10
    let (x, y) = (3, 4)
    print("${name} ${count} ${x + y}")
}
```

Types are inferred, but you can write them down: `let xs: [Int] = []`.
Inner blocks can shadow names, and `let x = x + 1` makes a new `x`.

## Functions and closures

Parameters and return types are written out. The last expression of the
body is its value:

```sedge
fn hypot(a: Float, b: Float) -> Float {
    (a * a + b * b).sqrt()
}

fn greet(name: String) {
    print("hello, ${name}")
}

fn main() {
    greet("you")
    print(hypot(3.0, 4.0))
}
```

There are two forms of closure: `fn(params) => expression` and
`fn(params) { block }`. Parameter types are usually inferred from where
the closure is used. A closure captures `let` bindings by value and `var`
bindings by reference, so changes are shared:

```sedge
fn make_counter() -> fn() -> Int {
    var n = 0
    fn() {
        n += 1
        n
    }
}

fn main() {
    let next = make_counter()
    next()
    next()
    print(next())          // 3

    let scale = 10
    let times = fn(x: Int) => x * scale
    print([1, 2].map(times))
    print([1, 2].map(str))  // named functions are values too
}
```

## Control flow

`if` is an expression. Without an `else` its value is `()`:

```sedge
fn sign(n: Int) -> String {
    if n < 0 { "negative" } else if n == 0 { "zero" } else { "positive" }
}

fn main() {
    for i in 0..3 {
        print(i)
    }
    for x in [10, 20] {
        print(x)
    }
    var n = 10
    while n > 0 {
        n -= 3
        if n == 4 { continue }
        if n < 2 { break }
    }
    print(sign(n))
}
```

`for` loops over ranges (`a..b` excludes `b` and `a..=b` includes it) and
over arrays. `break` and `continue` work in both kinds of loop. To loop
over a map, use `m.entries()`, `m.keys()` or `m.values()`.

## Structs and methods

```sedge
struct Point {
    x: Float,
    y: Float,
}

impl Point {
    fn new(x: Float, y: Float) -> Point {
        Point { x, y }             // shorthand for x: x, y: y
    }

    fn dist(self, other: Point) -> Float {
        let dx = self.x - other.x
        let dy = self.y - other.y
        (dx * dx + dy * dy).sqrt()
    }

    fn moved(self, dx: Float) -> Self {
        Point { x: self.x + dx, y: self.y }
    }
}

fn main() {
    let a = Point.new(0.0, 0.0)
    let b = a.moved(3.0)
    print(a.dist(b))
    print(b)                       // Point { x: 3.0, y: 0.0 }
}
```

Functions without `self` are called on the type: `Point.new(...)`.
Structs, arrays and maps are reference types. Assigning one shares it,
and changes through any name are visible through all of them (`p.x = 1.0`
works even when `p` is a `let`). Use `.copy()` on arrays for an
independent copy.

## Enums and pattern matching

Each enum variant can carry values:

```sedge
enum Expr {
    Num(Int),
    Add(Expr, Expr),
    Mul(Expr, Expr),
    Neg(Expr),
}

fn eval(e: Expr) -> Int {
    match e {
        Num(n) => n,
        Add(a, b) => eval(a) + eval(b),
        Mul(a, b) => eval(a) * eval(b),
        Neg(inner) => -eval(inner),
    }
}

fn main() {
    print(eval(Mul(Add(Num(1), Num(2)), Neg(Num(4)))))   // -12
}
```

Variants are referred to by name (`Num(1)`), or as `Expr.Num(1)` when
two enums use the same variant name.

Patterns can be:

| Pattern | Matches |
|---|---|
| `_` | anything |
| `name` | anything, and binds it |
| `42`, `"text"`, `true`, `-1` | that literal |
| `Some(p)`, `Shape.Circle(p)` | a variant, with patterns for its fields |
| `Point { x, y: 0.0, .. }` | a struct; `..` skips the remaining fields |
| `(p, q)` | a tuple |
| `p \| q` | either pattern (both must bind the same names) |

An arm can have a guard: `Some(n) if n > 0 => ...`. `let`, `for` and
parameters accept patterns that cannot fail, such as `let (a, b) = pair`.

The checker proves that every `match` handles every value. If one
doesn't, the error names a value it misses:

```
error: this `match` does not handle every case
  |
4 |     match pair {
  |           ^^^^ `(false, false)` is not covered
```

Arms that can never match are reported as warnings.

## Generics and bounds

Functions, structs and enums take type parameters in square brackets:

```sedge
struct Stack[T] {
    items: [T],
}

impl Stack[T] {
    fn new() -> Stack[T] { Stack { items: [] } }
    fn push(self, x: T) { self.items.push(x) }
    fn pop(self) -> Option[T] { self.items.pop() }
}

fn largest[T: Ord](xs: [T]) -> Option[T] {
    var best = xs.first()?
    for x in xs {
        if x > best { best = x }
    }
    Some(best)
}

fn sum[T: Num](xs: [T], zero: T) -> T {
    xs.fold(zero, fn(a, b) => a + b)
}

fn main() {
    let s = Stack.new()
    s.push("a")
    print(s.pop())
    print(largest([3, 9, 2]))
    print(sum([0.5, 0.25], 0.0))
}
```

Type arguments are always inferred. The bounds are:

- `Ord`: `<`, `<=`, `>`, `>=` and sorting. Satisfied by `Int`, `Float`,
  `String`, and tuples and arrays of them.
- `Num`: arithmetic. Satisfied by `Int` and `Float`.
- `Key`: can be a map key. Satisfied by `Int`, `String` and `Bool`.

A method can add a bound to its `impl`'s parameter with `where`:
`fn sorted(self) -> [T] where T: Ord`.

## Option, Result and `?`

There is no null. Optional values are `Option[T]` (`Some(x)` or `None`),
and operations that can fail return `Result[T, E]` (`Ok(x)` or `Err(e)`).
Inside a function that returns an `Option`, `expr?` unwraps a `Some` or
returns `None` at once. It works the same way for `Result`, provided the
error types match:

```sedge
enum ConfigError { Missing(String), NotANumber(String) }

fn lookup(m: Map[String, String], key: String) -> Result[String, ConfigError] {
    match m.get(key) {
        Some(v) => Ok(v),
        None => Err(Missing(key)),
    }
}

fn port(m: Map[String, String]) -> Result[Int, ConfigError] {
    let text = lookup(m, "port")?
    match text.parse_int() {
        Some(n) => Ok(n),
        None => Err(NotANumber(text)),
    }
}

fn main() {
    let m = Map.new()
    m["port"] = "8080"
    print(port(m))                         // Ok(8080)
    print(port(Map.new()))                 // Err(Missing("port"))
    print(Some(2).map(fn(x) => x * 10))    // Some(20)
    print(None.unwrap_or(5))               // 5
}
```

## Arrays and maps

```sedge
fn main() {
    var xs = [5, 3, 8]
    xs.push(1)
    xs[0] = 50
    xs.sort()
    print(xs)                                  // [1, 3, 8, 50]
    print(xs.map(fn(x) => x * 2).filter(fn(x) => x > 10))
    print(xs.enumerate())
    xs.sort_by(fn(a, b) => b - a)              // descending

    let ages = Map.new()
    ages["ana"] = 31
    ages.set("bo", 25)
    print(ages.get("ana"))                     // Some(31)
    print(ages.get_or("cy", 0))                // 0
    for (name, age) in ages.entries() {
        print("${name}: ${age}")
    }
}
```

`xs[i]` panics when `i` is out of bounds, and `m[k]` panics when the key
is missing. `xs.get(i)` and `m.get(k)` return an `Option` instead.

## Strings

Strings are immutable UTF-8. `${expr}` interpolates any value. `len()` and
`slice(start, end)` count bytes, and `chars()` splits the string into
characters:

```sedge
fn main() {
    let s = "Hello, wörld"
    print(s.len())                 // 13 bytes
    print(s.chars().len())         // 12 characters
    print(s.to_upper())
    print(s.split(", "))
    print(s.find("w"))             // Some(7)
    print("  x ".trim() + "!")
    print("${1 + 1} items")
    print("42".parse_int())        // Some(42)
}
```

## Tests

`test` blocks sit next to the code they test. `sedge run` skips them and
`sedge test file.sg` runs each one on its own:

```sedge
fn double(x: Int) -> Int { x * 2 }

test "double doubles" {
    assert_eq(double(21), 42)
}

test "negative numbers" {
    assert(double(-1) < 0)
}

fn main() {}
```

```
$ sedge test math.sg
ok    double doubles
ok    negative numbers

2 passed, 0 failed
```

## Runtime errors

Integer overflow, division by zero, out-of-bounds indexes, missing map
keys, `unwrap` on `None` and calls to `panic(...)` stop the program with
a message and a stack trace:

```
panic: index 5 is out of bounds for an array of length 3
    at get (index.sg:1)
    at main (index.sg:5)
```

The process then exits with status 101. `exit(code)` ends it with a
status of your choice.

## The prelude

These are available everywhere. The full list, with documentation, is in
[`prelude.sg`](../crates/sedge_check/src/prelude.sg):

- **Output and input:** `print`, `write`, `read_line`, `read_file`,
  `write_file`, `args`, `exit`, `clock`.
- **Conversion:** `str(x)` (display form), `debug(x)` (with quotes).
- **Checks:** `assert`, `assert_eq`, `panic`.
- **Numbers:** `min`, `max`, `random`, `random_int`, `seed_random`; Int
  methods (`abs`, `pow`, `to_float`, `to_base`, `clamp`, ...); Float
  methods (`sqrt`, `floor`, `sin`, `pow`, `fixed`, ...).
- **Strings:** `len`, `chars`, `slice`, `split`, `lines`, `trim`, `find`,
  `contains`, `replace`, `repeat`, `parse_int`, `parse_float`, ...
- **Arrays:** `push`, `pop`, `insert`, `remove`, `slice`, `sort`,
  `sort_by`, `sorted`, `map`, `filter`, `fold`, `each`, `any`, `all`,
  `find`, `position`, `count`, `enumerate`, `zip`, `flat_map`, `take`,
  `skip`, `join`, `min`, `max`, `contains`, `index_of`, `Array.filled`.
- **Maps:** `Map.new`, `get`, `set`, `get_or`, `update`, `contains`,
  `remove`, `keys`, `values`, `entries`.
- **Option and Result:** `unwrap`, `expect`, `unwrap_or`, `map`,
  `and_then`, `or`, `ok_or`, `map_err`, `ok`, `is_some`, `is_ok`, ...
