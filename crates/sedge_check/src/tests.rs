use super::*;

fn run(src: &str) -> Analysis {
    analyze(&[("test.sg".to_string(), src.to_string())])
}

fn ok(src: &str) -> Analysis {
    let a = run(src);
    let errors: Vec<_> = a.diagnostics.iter().filter(|d| d.is_error()).collect();
    assert!(errors.is_empty(), "unexpected errors:\n{}", a.render_diagnostics(false));
    a
}

/// Asserts that checking fails with a message containing `needle`.
fn fails(src: &str, needle: &str) {
    let a = run(src);
    let text = a.render_diagnostics(false);
    assert!(
        a.diagnostics.iter().any(|d| d.is_error() && (d.message.contains(needle) || d.render(&a.map, false).contains(needle))),
        "expected an error containing {needle:?}, got:\n{text}"
    );
}

fn warns(src: &str, needle: &str) {
    let a = run(src);
    assert!(
        a.diagnostics
            .iter()
            .any(|d| !d.is_error() && d.message.contains(needle)),
        "expected a warning containing {needle:?}, got:\n{}",
        a.render_diagnostics(false)
    );
}

#[test]
fn prelude_checks_cleanly() {
    let a = ok("fn main() {}");
    assert!(a.diagnostics.is_empty(), "{}", a.render_diagnostics(false));
}

#[test]
fn inference_through_generics_and_lambdas() {
    let a = ok("fn main() {
        let xs = [1, 2, 3]
        let ys = xs.map(fn(x) => x.to_float() * 1.5)
        let total = ys.fold(0.0, fn(acc, y) => acc + y)
        let names = Map.new()
        names.set(\"a\", total)
        let first = names.get(\"a\").unwrap_or(0.0)
        print(first)
    }");
    let total = a.typed.locals.iter().rev().find(|l| l.name == "total").unwrap();
    assert_eq!(total.ty, Ty::Float);
    let names = a.typed.locals.iter().rev().find(|l| l.name == "names").unwrap();
    assert_eq!(a.typed.show(&names.ty), "Map[String, Float]");
}

#[test]
fn structs_enums_and_methods() {
    ok("struct Point { x: Float, y: Float }
        impl Point {
            fn new(x: Float, y: Float) -> Point { Point { x, y } }
            fn norm(self) -> Float { (self.x * self.x + self.y * self.y).sqrt() }
            fn scale(self, k: Float) -> Self { Point { x: self.x * k, y: self.y * k } }
        }
        enum Shape { Circle(Point, Float), Rect(Point, Point), Empty }
        fn area(s: Shape) -> Float {
            match s {
                Circle(_, r) => 3.14 * r * r,
                Rect(a, b) => (b.x - a.x) * (b.y - a.y),
                Empty => 0.0,
            }
        }
        fn main() { print(area(Shape.Circle(Point.new(0.0, 0.0).scale(2.0), 1.0))) }");
}

#[test]
fn generic_struct_and_bounds() {
    ok("struct Pair[A, B] { first: A, second: B }
        impl Pair[A, B] {
            fn swap(self) -> Pair[B, A] { Pair { first: self.second, second: self.first } }
        }
        fn largest[T: Ord](xs: [T]) -> Option[T] { xs.max() }
        fn sum[T: Num](xs: [T], zero: T) -> T { xs.fold(zero, fn(a, b) => a + b) }
        fn main() {
            let p = Pair { first: 1, second: \"x\" }.swap()
            let s: String = p.first
            print(largest([3, 1, 2]))
            print(sum([1.5, 2.5], 0.0))
        }");
}

#[test]
fn closures_capture_and_box_vars() {
    let a = ok("fn counter() -> fn() -> Int {
            var n = 0
            fn() { n += 1; n }
        }
        fn main() { let c = counter(); print(c()) }");
    let n = a.typed.locals.iter().rev().find(|l| l.name == "n").unwrap();
    assert!(n.captured && n.boxed);
}

#[test]
fn try_operator() {
    ok("fn parse(s: String) -> Option[Int] {
            let n = s.parse_int()?
            Some(n * 2)
        }
        fn read(path: String) -> Result[Int, String] {
            let text = read_file(path)?
            Ok(text.len())
        }
        fn main() {}");
    fails(
        "fn f(s: String) -> Int { s.parse_int()? }\nfn main() {}",
        "needs the function to return an `Option`",
    );
}

#[test]
fn mismatches() {
    fails("fn main() { let x: Int = \"a\" }", "expected `Int`, found `String`");
    fails("fn main() { let x = 1 + 1.0 }", "expected `Int`, found `Float`");
    fails("fn f() -> Int { }\nfn main() {}", "expected `Int`, found `()`");
    fails("fn main() { let x: Int = if true { 1 } }", "`if` without `else`");
    fails("fn main() { print(nope) }", "cannot find `nope`");
    fails("fn main() { let count = 1\n print(cuont) }", "did you mean `count`?");
    fails("fn main() { let x = 1\n x = 2 }", "cannot assign twice");
    fails("fn main() { break }", "outside of a loop");
    fails("fn main() { [1, 2].len(3) }", "takes 0 arguments but 1 was supplied");
    fails("fn main() { let s = \"a\"\n s.nope() }", "no method named `nope`");
}

#[test]
fn operator_obligations() {
    fails("fn main() { let x = true + false }", "needs `Int`, `Float` or `String`");
    fails("fn f[T](a: T, b: T) -> Bool { a < b }\nfn main() {}", "`T: Ord`");
    fails("struct P { x: Int }\nfn main() { let a = P { x: 1 } < P { x: 2 } }", "`Ord`");
    ok("fn main() { let f = fn(a, b) => a * b\n print(f(2, 3)) }");
}

#[test]
fn struct_literal_errors() {
    fails("struct P { x: Int, y: Int }\nfn main() { let p = P { x: 1 } }", "missing field `y`");
    fails("struct P { x: Int }\nfn main() { let p = P { x: 1, z: 2 } }", "has no field `z`");
    fails("struct P { x: Int }\nfn main() { let p = P { x: 1 }\n print(p.y) }", "has no field `y`");
}

#[test]
fn exhaustiveness() {
    fails(
        "enum E { A, B(Int), C }\nfn f(e: E) -> Int { match e { A => 1, B(_) => 2 } }\nfn main() {}",
        "`C` is not covered",
    );
    fails(
        "fn f(x: Option[Bool]) -> Int { match x { Some(true) => 1, None => 0 } }\nfn main() {}",
        "`Some(false)` is not covered",
    );
    fails(
        "fn f(x: (Bool, Bool)) -> Int { match x { (true, _) => 1, (_, true) => 2 } }\nfn main() {}",
        "`(false, false)` is not covered",
    );
    fails("fn f(n: Int) -> Int { match n { 0 => 1, 1 => 2 } }\nfn main() {}", "`_` is not covered");
    fails(
        "fn f(n: Option[Int]) -> Int { match n { Some(x) if x > 0 => x, None => 0 } }\nfn main() {}",
        "`Some(_)` is not covered",
    );
    ok("enum E { A, B, C }\nfn f(e: E) -> Int { match e { A | B => 1, C => 2 } }\nfn main() {}");
    ok("fn f(x: (Int, String)) -> Int { match x { (0, \"a\") => 1, (n, _) => n } }\nfn main() {}");
}

#[test]
fn unreachable_arms() {
    warns(
        "fn f(x: Option[Int]) -> Int { match x { _ => 1, Some(_) => 2 } }\nfn main() {}",
        "unreachable pattern",
    );
    warns(
        "fn f(b: Bool) -> Int { match b { true => 1, false => 2, true => 3 } }\nfn main() {}",
        "unreachable pattern",
    );
}

#[test]
fn refutable_let() {
    fails(
        "fn main() { let Some(x) = Some(1) }",
        "does not match every value",
    );
    ok("struct P { x: Int, y: Int }\nfn main() { let P { x, y } = P { x: 1, y: 2 }\n let (a, b) = (x, y)\n print(a + b) }");
}

#[test]
fn or_patterns_bind_consistently() {
    ok("enum E { A(Int), B(Int) }\nfn f(e: E) -> Int { match e { A(n) | B(n) => n } }\nfn main() {}");
    fails(
        "enum E { A(Int), B(Int) }\nfn f(e: E) -> Int { match e { A(n) | B(_) => 1 } }\nfn main() {}",
        "must be bound in every alternative",
    );
}

#[test]
fn recursion_and_mutual_recursion() {
    ok("fn even(n: Int) -> Bool { if n == 0 { true } else { odd(n - 1) } }
        fn odd(n: Int) -> Bool { if n == 0 { false } else { even(n - 1) } }
        enum List[T] { Cons(T, List[T]), Nil }
        fn len[T](l: List[T]) -> Int { match l { Cons(_, rest) => 1 + len(rest), Nil => 0 } }
        fn main() { print(len(Cons(1, Cons(2, Nil)))) }");
}

#[test]
fn methods_on_builtins_only_in_prelude() {
    fails("impl Int { fn double(self) -> Int { self * 2 } }\nfn main() {}", "only be added to types declared");
}

#[test]
fn unused_values_warn() {
    warns("fn main() { let x = 1\n x == 2 }", "unused comparison");
}

#[test]
fn hover_index_records_uses() {
    let a = ok("fn add(a: Int, b: Int) -> Int { a + b }\nfn main() { let s = add(1, 2)\n print(s) }");
    let uses_of_add = a
        .typed
        .uses
        .iter()
        .filter(|u| matches!(u.def, Def::Fn(f) if a.typed.fns[f as usize].name == "add"))
        .count();
    assert_eq!(uses_of_add, 2, "definition and call");
}

