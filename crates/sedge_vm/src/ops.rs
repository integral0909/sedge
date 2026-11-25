//! Structural equality, ordering and formatting of values.

use crate::bytecode::TypeInfo;
use crate::map::Key;
use crate::value::{Heap, Object, Value};
use std::cmp::Ordering;
use std::fmt::Write;

/// Nesting deeper than this prints as `...`, which also stops cycles.
const MAX_DEPTH: usize = 48;

pub fn equal(heap: &Heap, a: Value, b: Value) -> bool {
    equal_at(heap, a, b, 0)
}

fn equal_at(heap: &Heap, a: Value, b: Value, depth: usize) -> bool {
    if depth > 256 {
        return false;
    }
    match (a, b) {
        (Value::Unit, Value::Unit) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Tag { adt: a1, tag: t1 }, Value::Tag { adt: a2, tag: t2 }) => a1 == a2 && t1 == t2,
        (Value::Obj(x), Value::Obj(y)) => {
            if x == y {
                return true;
            }
            let all = |xs: &[Value], ys: &[Value]| {
                xs.len() == ys.len()
                    && xs
                        .iter()
                        .zip(ys)
                        .all(|(p, q)| equal_at(heap, *p, *q, depth + 1))
            };
            match (heap.get(x), heap.get(y)) {
                (Object::Str(p), Object::Str(q)) => p == q,
                (Object::Array(p), Object::Array(q)) => all(p, q),
                (Object::Tuple(p), Object::Tuple(q)) => all(p, q),
                (Object::Struct { adt: a1, fields: p }, Object::Struct { adt: a2, fields: q }) => {
                    a1 == a2 && all(p, q)
                }
                (
                    Object::Variant {
                        adt: a1,
                        tag: t1,
                        fields: p,
                    },
                    Object::Variant {
                        adt: a2,
                        tag: t2,
                        fields: q,
                    },
                ) => a1 == a2 && t1 == t2 && all(p, q),
                (Object::Map(p), Object::Map(q)) => {
                    p.len() == q.len()
                        && p.iter().all(|(k, v)| {
                            key_of(heap, k)
                                .and_then(|key| q.get(&key))
                                .is_some_and(|w| equal_at(heap, v, w, depth + 1))
                        })
                }
                (Object::Range(a1, b1), Object::Range(a2, b2)) => a1 == a2 && b1 == b2,
                (Object::Cell(p), Object::Cell(q)) => equal_at(heap, *p, *q, depth + 1),
                _ => false,
            }
        }
        _ => false,
    }
}

/// Ordering for `Ord` types: numbers, strings, and tuples and arrays of
/// them, compared lexicographically. Floats use IEEE total order so sorting
/// is well defined even with NaN.
pub fn compare(heap: &Heap, a: Value, b: Value) -> Ordering {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x.cmp(&y),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(&y).unwrap_or_else(|| x.total_cmp(&y)),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(&y),
        (Value::Obj(x), Value::Obj(y)) => {
            let seq = |xs: &[Value], ys: &[Value]| {
                for (p, q) in xs.iter().zip(ys) {
                    let o = compare(heap, *p, *q);
                    if o != Ordering::Equal {
                        return o;
                    }
                }
                xs.len().cmp(&ys.len())
            };
            match (heap.get(x), heap.get(y)) {
                (Object::Str(p), Object::Str(q)) => p.cmp(q),
                (Object::Tuple(p), Object::Tuple(q)) => seq(p, q),
                (Object::Array(p), Object::Array(q)) => seq(p, q),
                _ => Ordering::Equal,
            }
        }
        _ => Ordering::Equal,
    }
}

pub fn key_of(heap: &Heap, v: Value) -> Option<Key> {
    match v {
        Value::Int(i) => Some(Key::Int(i)),
        Value::Bool(b) => Some(Key::Bool(b)),
        Value::Obj(g) => match heap.get(g) {
            Object::Str(s) => Some(Key::Str(s.clone())),
            _ => None,
        },
        _ => None,
    }
}

pub fn format_float(f: f64) -> String {
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    format!("{f:?}")
}

/// The display form: strings without quotes at the top level, quoted
/// inside containers.
pub fn display(heap: &Heap, types: &[TypeInfo], protos: &[String], v: Value) -> String {
    if let Value::Obj(g) = v {
        if let Object::Str(s) = heap.get(g) {
            return s.clone();
        }
    }
    debug(heap, types, protos, v)
}

/// The debug form, with strings quoted.
pub fn debug(heap: &Heap, types: &[TypeInfo], protos: &[String], v: Value) -> String {
    let mut out = String::new();
    write_value(&mut out, heap, types, protos, v, 0);
    out
}

fn write_seq(
    out: &mut String,
    heap: &Heap,
    types: &[TypeInfo],
    protos: &[String],
    vs: &[Value],
    depth: usize,
) {
    for (i, x) in vs.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        write_value(out, heap, types, protos, *x, depth + 1);
    }
}

fn write_value(out: &mut String, heap: &Heap, types: &[TypeInfo], protos: &[String], v: Value, depth: usize) {
    if depth > MAX_DEPTH {
        out.push_str("...");
        return;
    }
    match v {
        Value::Unit => out.push_str("()"),
        Value::Bool(b) => {
            let _ = write!(out, "{b}");
        }
        Value::Int(i) => {
            let _ = write!(out, "{i}");
        }
        Value::Float(f) => out.push_str(&format_float(f)),
        Value::Tag { adt, tag } => out.push_str(&types[adt as usize].variants[tag as usize]),
        Value::Obj(g) => match heap.get(g) {
            Object::Str(s) => {
                let _ = write!(out, "{s:?}");
            }
            Object::Array(vs) => {
                out.push('[');
                write_seq(out, heap, types, protos, vs, depth);
                out.push(']');
            }
            Object::Tuple(vs) => {
                out.push('(');
                write_seq(out, heap, types, protos, vs, depth);
                if vs.len() == 1 {
                    out.push(',');
                }
                out.push(')');
            }
            Object::Map(m) => {
                out.push('{');
                for (i, (k, val)) in m.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    write_value(out, heap, types, protos, k, depth + 1);
                    out.push_str(": ");
                    write_value(out, heap, types, protos, val, depth + 1);
                }
                out.push('}');
            }
            Object::Struct { adt, fields } => {
                let info = &types[*adt as usize];
                out.push_str(&info.name);
                if fields.is_empty() {
                    out.push_str(" {}");
                    return;
                }
                out.push_str(" { ");
                for (i, (name, x)) in info.fields.iter().zip(fields.iter()).enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(name);
                    out.push_str(": ");
                    write_value(out, heap, types, protos, *x, depth + 1);
                }
                out.push_str(" }");
            }
            Object::Variant { adt, tag, fields } => {
                out.push_str(&types[*adt as usize].variants[*tag as usize]);
                out.push('(');
                write_seq(out, heap, types, protos, fields, depth);
                out.push(')');
            }
            Object::Closure { func, .. } => {
                let _ = write!(out, "<fn {}>", protos[*func as usize]);
            }
            Object::Cell(x) => write_value(out, heap, types, protos, *x, depth),
            Object::Range(lo, hi) => {
                let _ = write!(out, "{lo}..{hi}");
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_nested_values() {
        let mut h = Heap::new();
        let types = vec![TypeInfo {
            name: "Option".into(),
            fields: vec![],
            variants: vec!["Some".into(), "None".into()],
        }];
        let s = h.new_str("hi");
        let some = Value::Obj(h.alloc(Object::Variant {
            adt: 0,
            tag: 0,
            fields: vec![s].into_boxed_slice(),
        }));
        let none = Value::Tag { adt: 0, tag: 1 };
        let t = h.new_tuple(vec![Value::Float(1.0), none]);
        let arr = h.new_array(vec![some, t, Value::Int(-3)]);
        assert_eq!(display(&h, &types, &[], arr), "[Some(\"hi\"), (1.0, None), -3]");
        assert_eq!(display(&h, &types, &[], s), "hi");
        assert_eq!(debug(&h, &types, &[], s), "\"hi\"");
    }

    #[test]
    fn structural_equality_and_order() {
        let mut h = Heap::new();
        let a = h.new_str("abc");
        let b = h.new_str("abc");
        let c = h.new_str("abd");
        assert!(equal(&h, a, b));
        assert!(!equal(&h, a, c));
        let t1 = h.new_tuple(vec![Value::Int(1), a]);
        let t2 = h.new_tuple(vec![Value::Int(1), c]);
        assert_eq!(compare(&h, t1, t2), Ordering::Less);
        assert!(!equal(&h, Value::Float(f64::NAN), Value::Float(f64::NAN)));
    }
}
