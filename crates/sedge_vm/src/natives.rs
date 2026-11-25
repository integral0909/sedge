//! Functions the prelude declares as `native`.

use crate::ops;
use crate::value::{Object, Value};
use crate::vm::{NativeError, Vm};
use std::io::Write;

pub type NativeFn = fn(&mut Vm, &[Value]) -> Result<Value, NativeError>;

type R = Result<Value, NativeError>;

fn panic<T>(msg: impl Into<String>) -> Result<T, NativeError> {
    Err(NativeError::Panic(msg.into()))
}

pub fn lookup(name: &str) -> Option<NativeFn> {
    Some(match name {
        "print" => print,
        "write" => write,
        "str" => str_,
        "debug" => debug,
        "panic" => panic_,
        "assert" => assert,
        "assert_eq" => assert_eq,
        "read_line" => read_line,
        "args" => args,
        "read_file" => read_file,
        "write_file" => write_file,
        "clock" => clock,
        "exit" => exit,
        "random" => random,
        "seed_random" => seed_random,

        "Int.to_float" => |_, a| Ok(Value::Float(a[0].as_int() as f64)),
        "Int.abs" => |_, a| match a[0].as_int().checked_abs() {
            Some(v) => Ok(Value::Int(v)),
            None => panic("integer overflow in `abs`"),
        },
        "Int.pow" => int_pow,
        "Int.to_char" => |vm, a| {
            let n = a[0].as_int();
            match u32::try_from(n).ok().and_then(char::from_u32) {
                Some(c) => Ok(vm.heap.new_str(c.to_string())),
                None => panic(format!("{n} is not a valid Unicode code point")),
            }
        },
        "Int.to_base" => int_to_base,

        "Float.to_int" => |_, a| {
            let f = a[0].as_float();
            if f.is_nan() || f < i64::MIN as f64 || f >= i64::MAX as f64 {
                return panic(format!(
                    "{} cannot be converted to Int",
                    ops::format_float(f)
                ));
            }
            Ok(Value::Int(f as i64))
        },
        "Float.sqrt" => |_, a| Ok(Value::Float(a[0].as_float().sqrt())),
        "Float.floor" => |_, a| Ok(Value::Float(a[0].as_float().floor())),
        "Float.ceil" => |_, a| Ok(Value::Float(a[0].as_float().ceil())),
        "Float.round" => |_, a| Ok(Value::Float(a[0].as_float().round())),
        "Float.abs" => |_, a| Ok(Value::Float(a[0].as_float().abs())),
        "Float.sin" => |_, a| Ok(Value::Float(a[0].as_float().sin())),
        "Float.cos" => |_, a| Ok(Value::Float(a[0].as_float().cos())),
        "Float.tan" => |_, a| Ok(Value::Float(a[0].as_float().tan())),
        "Float.atan2" => |_, a| Ok(Value::Float(a[0].as_float().atan2(a[1].as_float()))),
        "Float.exp" => |_, a| Ok(Value::Float(a[0].as_float().exp())),
        "Float.ln" => |_, a| Ok(Value::Float(a[0].as_float().ln())),
        "Float.pow" => |_, a| Ok(Value::Float(a[0].as_float().powf(a[1].as_float()))),
        "Float.is_nan" => |_, a| Ok(Value::Bool(a[0].as_float().is_nan())),
        "Float.fixed" => |vm, a| {
            let d = a[1].as_int().clamp(0, 30) as usize;
            Ok(vm.heap.new_str(format!("{:.*}", d, a[0].as_float())))
        },

        "String.len" => |vm, a| Ok(Value::Int(vm.heap.str(a[0]).len() as i64)),
        "String.chars" => string_chars,
        "String.byte_at" => |vm, a| {
            let s = vm.heap.str(a[0]);
            let i = a[1].as_int();
            match usize::try_from(i).ok().and_then(|i| s.as_bytes().get(i)) {
                Some(b) => Ok(Value::Int(*b as i64)),
                None => panic(format!(
                    "byte index {i} is out of bounds for a string of length {}",
                    s.len()
                )),
            }
        },
        "String.code" => |vm, a| match vm.heap.str(a[0]).chars().next() {
            Some(c) => Ok(Value::Int(c as i64)),
            None => panic("`code` on an empty string"),
        },
        "String.slice" => string_slice,
        "String.find" => |vm, a| {
            let found = vm.heap.str(a[0]).find(vm.heap.str(a[1]));
            Ok(match found {
                Some(i) => vm.some(Value::Int(i as i64)),
                None => vm.none(),
            })
        },
        "String.contains" => |vm, a| Ok(Value::Bool(vm.heap.str(a[0]).contains(vm.heap.str(a[1])))),
        "String.starts_with" => |vm, a| {
            Ok(Value::Bool(
                vm.heap.str(a[0]).starts_with(vm.heap.str(a[1])),
            ))
        },
        "String.ends_with" => {
            |vm, a| Ok(Value::Bool(vm.heap.str(a[0]).ends_with(vm.heap.str(a[1]))))
        }
        "String.split" => string_split,
        "String.lines" => |vm, a| {
            let parts: Vec<String> = vm.heap.str(a[0]).lines().map(String::from).collect();
            Ok(str_array(vm, parts))
        },
        "String.trim" => |vm, a| {
            let s = vm.heap.str(a[0]).trim().to_string();
            Ok(vm.heap.new_str(s))
        },
        "String.to_upper" => |vm, a| {
            let s = vm.heap.str(a[0]).to_uppercase();
            Ok(vm.heap.new_str(s))
        },
        "String.to_lower" => |vm, a| {
            let s = vm.heap.str(a[0]).to_lowercase();
            Ok(vm.heap.new_str(s))
        },
        "String.replace" => |vm, a| {
            let s = vm
                .heap
                .str(a[0])
                .replace(vm.heap.str(a[1]), vm.heap.str(a[2]));
            Ok(vm.heap.new_str(s))
        },
        "String.repeat" => |vm, a| {
            let n = a[1].as_int();
            if n < 0 {
                return panic(format!("cannot repeat a string {n} times"));
            }
            let s = vm.heap.str(a[0]);
            if s.len().saturating_mul(n as usize) > 1 << 30 {
                return panic("repeated string would be too large");
            }
            let s = s.repeat(n as usize);
            Ok(vm.heap.new_str(s))
        },
        "String.parse_int" => |vm, a| {
            let parsed = vm.heap.str(a[0]).trim().parse::<i64>().ok();
            Ok(match parsed {
                Some(i) => vm.some(Value::Int(i)),
                None => vm.none(),
            })
        },
        "String.parse_float" => |vm, a| {
            let parsed = vm.heap.str(a[0]).trim().parse::<f64>().ok();
            Ok(match parsed {
                Some(f) => vm.some(Value::Float(f)),
                None => vm.none(),
            })
        },

        "Array.filled" => |vm, a| {
            let n = a[1].as_int();
            if !(0..=100_000_000).contains(&n) {
                return panic(format!("cannot make an array of length {n}"));
            }
            Ok(vm.heap.new_array(vec![a[0]; n as usize]))
        },
        "Array.len" => |vm, a| Ok(Value::Int(vm.heap.array(a[0]).len() as i64)),
        "Array.push" => |vm, a| {
            vm.heap.array_mut(a[0]).push(a[1]);
            Ok(Value::Unit)
        },
        "Array.pop" => |vm, a| {
            let v = vm.heap.array_mut(a[0]).pop();
            Ok(match v {
                Some(v) => vm.some(v),
                None => vm.none(),
            })
        },
        "Array.insert" => |vm, a| {
            let i = a[1].as_int();
            let arr = vm.heap.array_mut(a[0]);
            match usize::try_from(i) {
                Ok(i) if i <= arr.len() => {
                    arr.insert(i, a[2]);
                    Ok(Value::Unit)
                }
                _ => panic(format!(
                    "insertion index {i} is out of bounds for an array of length {}",
                    arr.len()
                )),
            }
        },
        "Array.remove" => |vm, a| {
            let i = a[1].as_int();
            let arr = vm.heap.array_mut(a[0]);
            match usize::try_from(i) {
                Ok(i) if i < arr.len() => Ok(arr.remove(i)),
                _ => panic(format!(
                    "index {i} is out of bounds for an array of length {}",
                    arr.len()
                )),
            }
        },
        "Array.clear" => |vm, a| {
            vm.heap.array_mut(a[0]).clear();
            Ok(Value::Unit)
        },
        "Array.slice" => |vm, a| {
            let arr = vm.heap.array(a[0]);
            let (lo, hi) = (a[1].as_int(), a[2].as_int());
            if lo < 0 || hi < lo || hi as usize > arr.len() {
                return panic(format!(
                    "slice {lo}..{hi} is out of bounds for an array of length {}",
                    arr.len()
                ));
            }
            let part = arr[lo as usize..hi as usize].to_vec();
            Ok(vm.heap.new_array(part))
        },
        "Array.reverse" => |vm, a| {
            vm.heap.array_mut(a[0]).reverse();
            Ok(Value::Unit)
        },
        "Array.copy" => |vm, a| {
            let items = vm.heap.array(a[0]).clone();
            Ok(vm.heap.new_array(items))
        },
        "Array.concat" => |vm, a| {
            let mut items = vm.heap.array(a[0]).clone();
            items.extend_from_slice(vm.heap.array(a[1]));
            Ok(vm.heap.new_array(items))
        },
        "Array.join" => |vm, a| {
            let sep = vm.heap.str(a[1]).to_string();
            let parts: Vec<String> = vm.heap.array(a[0]).iter().map(|v| vm.display(*v)).collect();
            Ok(vm.heap.new_str(parts.join(&sep)))
        },
        "Array.contains" => |vm, a| {
            let found = vm
                .heap
                .array(a[0])
                .iter()
                .any(|v| ops::equal(&vm.heap, *v, a[1]));
            Ok(Value::Bool(found))
        },
        "Array.index_of" => |vm, a| {
            let found = vm
                .heap
                .array(a[0])
                .iter()
                .position(|v| ops::equal(&vm.heap, *v, a[1]));
            Ok(match found {
                Some(i) => vm.some(Value::Int(i as i64)),
                None => vm.none(),
            })
        },
        "Array.sort" => |vm, a| {
            let mut items = std::mem::take(vm.heap.array_mut(a[0]));
            items.sort_by(|x, y| ops::compare(&vm.heap, *x, *y));
            *vm.heap.array_mut(a[0]) = items;
            Ok(Value::Unit)
        },

        "Map.new" => |vm, _| Ok(Value::Obj(vm.heap.alloc(Object::Map(Default::default())))),
        "Map.len" => |vm, a| Ok(Value::Int(vm.heap.map(a[0]).len() as i64)),
        "Map.get" => |vm, a| {
            let key = ops::key_of(&vm.heap, a[1]).expect("map key");
            let v = vm.heap.map(a[0]).get(&key);
            Ok(match v {
                Some(v) => vm.some(v),
                None => vm.none(),
            })
        },
        "Map.set" => |vm, a| {
            let key = ops::key_of(&vm.heap, a[1]).expect("map key");
            vm.heap.map_mut(a[0]).insert(key, a[1], a[2]);
            Ok(Value::Unit)
        },
        "Map.contains" => |vm, a| {
            let key = ops::key_of(&vm.heap, a[1]).expect("map key");
            Ok(Value::Bool(vm.heap.map(a[0]).get(&key).is_some()))
        },
        "Map.remove" => |vm, a| {
            let key = ops::key_of(&vm.heap, a[1]).expect("map key");
            let v = vm.heap.map_mut(a[0]).remove(&key);
            Ok(match v {
                Some(v) => vm.some(v),
                None => vm.none(),
            })
        },
        "Map.keys" => |vm, a| {
            let ks: Vec<Value> = vm.heap.map(a[0]).iter().map(|(k, _)| k).collect();
            Ok(vm.heap.new_array(ks))
        },
        "Map.values" => |vm, a| {
            let vs: Vec<Value> = vm.heap.map(a[0]).iter().map(|(_, v)| v).collect();
            Ok(vm.heap.new_array(vs))
        },
        "Map.entries" => |vm, a| {
            let pairs: Vec<(Value, Value)> = vm.heap.map(a[0]).iter().collect();
            let mut out = Vec::with_capacity(pairs.len());
            for (k, v) in pairs {
                out.push(vm.heap.new_tuple(vec![k, v]));
            }
            Ok(vm.heap.new_array(out))
        },
        "Map.clear" => |vm, a| {
            vm.heap.map_mut(a[0]).clear();
            Ok(Value::Unit)
        },
        _ => return None,
    })
}

fn str_array(vm: &mut Vm, parts: Vec<String>) -> Value {
    let items: Vec<Value> = parts.into_iter().map(|p| vm.heap.new_str(p)).collect();
    vm.heap.new_array(items)
}

fn print(vm: &mut Vm, a: &[Value]) -> R {
    let s = vm.display(a[0]);
    let _ = writeln!(vm.out, "{s}");
    Ok(Value::Unit)
}

fn write(vm: &mut Vm, a: &[Value]) -> R {
    let s = vm.display(a[0]);
    let _ = write!(vm.out, "{s}");
    Ok(Value::Unit)
}

fn str_(vm: &mut Vm, a: &[Value]) -> R {
    if let Value::Obj(g) = a[0] {
        if matches!(vm.heap.get(g), Object::Str(_)) {
            return Ok(a[0]);
        }
    }
    let s = vm.display(a[0]);
    Ok(vm.heap.new_str(s))
}

fn debug(vm: &mut Vm, a: &[Value]) -> R {
    let s = vm.debug(a[0]);
    Ok(vm.heap.new_str(s))
}

fn panic_(vm: &mut Vm, a: &[Value]) -> R {
    panic(vm.heap.str(a[0]).to_string())
}

fn assert(_: &mut Vm, a: &[Value]) -> R {
    if a[0].as_bool() {
        Ok(Value::Unit)
    } else {
        panic("assertion failed")
    }
}

fn assert_eq(vm: &mut Vm, a: &[Value]) -> R {
    if ops::equal(&vm.heap, a[0], a[1]) {
        Ok(Value::Unit)
    } else {
        panic(format!(
            "assertion failed: values are not equal\n  actual:   {}\n  expected: {}",
            vm.debug(a[0]),
            vm.debug(a[1])
        ))
    }
}

fn read_line(vm: &mut Vm, _: &[Value]) -> R {
    let _ = vm.out.flush();
    let mut line = String::new();
    match vm.input.read_line(&mut line) {
        Ok(0) | Err(_) => Ok(vm.none()),
        Ok(_) => {
            let trimmed = line.trim_end_matches(['\n', '\r']).to_string();
            let s = vm.heap.new_str(trimmed);
            Ok(vm.some(s))
        }
    }
}

fn args(vm: &mut Vm, _: &[Value]) -> R {
    let a = vm.args.clone();
    Ok(str_array(vm, a))
}

fn read_file(vm: &mut Vm, a: &[Value]) -> R {
    let path = vm.heap.str(a[0]).to_string();
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let s = vm.heap.new_str(text);
            Ok(vm.ok(s))
        }
        Err(e) => {
            let s = vm.heap.new_str(format!("{path}: {e}"));
            Ok(vm.err(s))
        }
    }
}

fn write_file(vm: &mut Vm, a: &[Value]) -> R {
    let path = vm.heap.str(a[0]).to_string();
    let result = std::fs::write(&path, vm.heap.str(a[1]));
    match result {
        Ok(()) => Ok(vm.ok(Value::Unit)),
        Err(e) => {
            let s = vm.heap.new_str(format!("{path}: {e}"));
            Ok(vm.err(s))
        }
    }
}

fn clock(vm: &mut Vm, _: &[Value]) -> R {
    Ok(Value::Float(vm.started.elapsed().as_secs_f64()))
}

fn exit(vm: &mut Vm, a: &[Value]) -> R {
    let _ = vm.out.flush();
    Err(NativeError::Exit(a[0].as_int() as i32))
}

/// xorshift64*: small, fast and good enough for games and simulations.
fn next_random(vm: &mut Vm) -> u64 {
    let mut x = vm.rng;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    vm.rng = x;
    x.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

fn random(vm: &mut Vm, _: &[Value]) -> R {
    let bits = next_random(vm) >> 11;
    Ok(Value::Float(bits as f64 / (1u64 << 53) as f64))
}

fn seed_random(vm: &mut Vm, a: &[Value]) -> R {
    let seed = a[0].as_int() as u64;
    vm.rng = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    Ok(Value::Unit)
}

fn int_pow(_: &mut Vm, a: &[Value]) -> R {
    let (b, e) = (a[0].as_int(), a[1].as_int());
    if e < 0 {
        return panic(format!("negative exponent {e} in `pow`"));
    }
    match u32::try_from(e).ok().and_then(|e| b.checked_pow(e)) {
        Some(v) => Ok(Value::Int(v)),
        None => panic("integer overflow in `pow`"),
    }
}

fn int_to_base(vm: &mut Vm, a: &[Value]) -> R {
    let (n, base) = (a[0].as_int(), a[1].as_int());
    if !(2..=36).contains(&base) {
        return panic(format!("base {base} is not between 2 and 36"));
    }
    let digits = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let neg = n < 0;
    let mut m = n.unsigned_abs();
    let mut out = Vec::new();
    loop {
        out.push(digits[(m % base as u64) as usize]);
        m /= base as u64;
        if m == 0 {
            break;
        }
    }
    if neg {
        out.push(b'-');
    }
    out.reverse();
    Ok(vm.heap.new_str(String::from_utf8(out).unwrap()))
}

fn string_chars(vm: &mut Vm, a: &[Value]) -> R {
    let parts: Vec<String> = vm.heap.str(a[0]).chars().map(String::from).collect();
    Ok(str_array(vm, parts))
}

fn string_slice(vm: &mut Vm, a: &[Value]) -> R {
    let s = vm.heap.str(a[0]);
    let (lo, hi) = (a[1].as_int(), a[2].as_int());
    if lo < 0 || hi < lo || hi as usize > s.len() {
        return panic(format!(
            "slice {lo}..{hi} is out of bounds for a string of length {}",
            s.len()
        ));
    }
    match s.get(lo as usize..hi as usize) {
        Some(part) => {
            let part = part.to_string();
            Ok(vm.heap.new_str(part))
        }
        None => panic(format!(
            "slice {lo}..{hi} does not fall on character boundaries"
        )),
    }
}

fn string_split(vm: &mut Vm, a: &[Value]) -> R {
    let s = vm.heap.str(a[0]);
    let sep = vm.heap.str(a[1]);
    let parts: Vec<String> = if sep.is_empty() {
        s.chars().map(String::from).collect()
    } else {
        s.split(sep).map(String::from).collect()
    };
    Ok(str_array(vm, parts))
}
