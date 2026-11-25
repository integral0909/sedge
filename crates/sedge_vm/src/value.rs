//! Values and the garbage-collected heap.
//!
//! A [`Value`] is a small copyable word: scalars are stored inline and
//! everything else is an index into the heap. Collection is mark and sweep
//! with precise roots, run only at safepoints (calls and backward jumps),
//! when every live value is in a register.

use crate::map::OrderedMap;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Gc(pub u32);

#[derive(Clone, Copy, Debug)]
pub enum Value {
    Unit,
    Bool(bool),
    Int(i64),
    Float(f64),
    /// An enum variant without fields.
    Tag { adt: u32, tag: u32 },
    Obj(Gc),
}

impl Value {
    pub fn as_int(self) -> i64 {
        match self {
            Value::Int(i) => i,
            other => panic!("expected Int, found {other:?}"),
        }
    }

    pub fn as_float(self) -> f64 {
        match self {
            Value::Float(f) => f,
            other => panic!("expected Float, found {other:?}"),
        }
    }

    pub fn as_bool(self) -> bool {
        match self {
            Value::Bool(b) => b,
            other => panic!("expected Bool, found {other:?}"),
        }
    }

    pub fn as_obj(self) -> Gc {
        match self {
            Value::Obj(g) => g,
            other => panic!("expected an object, found {other:?}"),
        }
    }
}

#[derive(Debug)]
pub enum Object {
    Str(String),
    Array(Vec<Value>),
    Map(OrderedMap),
    Struct { adt: u32, fields: Box<[Value]> },
    Variant { adt: u32, tag: u32, fields: Box<[Value]> },
    Tuple(Box<[Value]>),
    Closure { func: u32, captures: Box<[Value]> },
    Cell(Value),
    Range(i64, i64),
}

impl Object {
    fn trace(&self, out: &mut Vec<Gc>) {
        let mut push = |v: &Value| {
            if let Value::Obj(g) = v {
                out.push(*g);
            }
        };
        match self {
            Object::Str(_) | Object::Range(..) => {}
            Object::Array(vs) => vs.iter().for_each(push),
            Object::Map(m) => {
                for (k, v) in m.iter() {
                    push(&k);
                    push(&v);
                }
            }
            Object::Struct { fields, .. }
            | Object::Variant { fields, .. }
            | Object::Tuple(fields)
            | Object::Closure {
                captures: fields, ..
            } => fields.iter().for_each(push),
            Object::Cell(v) => push(v),
        }
    }

    /// A rough size, used to decide when to collect.
    fn weight(&self) -> usize {
        match self {
            Object::Str(s) => 1 + s.len() / 32,
            Object::Array(v) => 1 + v.len() / 4,
            Object::Map(m) => 1 + m.len() / 2,
            _ => 1,
        }
    }
}

struct Slot {
    obj: Option<Object>,
    marked: bool,
}

pub struct Heap {
    slots: Vec<Slot>,
    free: Vec<u32>,
    /// Weight allocated since the last collection.
    allocated: usize,
    threshold: usize,
    pub collections: usize,
    live: usize,
}

impl Default for Heap {
    fn default() -> Heap {
        Heap::new()
    }
}

const MIN_THRESHOLD: usize = 64 * 1024;

impl Heap {
    pub fn new() -> Heap {
        Heap {
            slots: Vec::new(),
            free: Vec::new(),
            allocated: 0,
            threshold: MIN_THRESHOLD,
            collections: 0,
            live: 0,
        }
    }

    pub fn alloc(&mut self, obj: Object) -> Gc {
        self.allocated += obj.weight();
        self.live += 1;
        if let Some(i) = self.free.pop() {
            self.slots[i as usize] = Slot {
                obj: Some(obj),
                marked: false,
            };
            Gc(i)
        } else {
            self.slots.push(Slot {
                obj: Some(obj),
                marked: false,
            });
            Gc(self.slots.len() as u32 - 1)
        }
    }

    pub fn get(&self, g: Gc) -> &Object {
        self.slots[g.0 as usize].obj.as_ref().expect("use after free")
    }

    pub fn get_mut(&mut self, g: Gc) -> &mut Object {
        self.slots[g.0 as usize].obj.as_mut().expect("use after free")
    }

    pub fn should_collect(&self) -> bool {
        self.allocated >= self.threshold
    }

    pub fn live_objects(&self) -> usize {
        self.live
    }

    /// Marks from `roots` and frees everything unreachable.
    pub fn collect(&mut self, roots: impl Iterator<Item = Value>) {
        let mut stack: Vec<Gc> = roots
            .filter_map(|v| match v {
                Value::Obj(g) => Some(g),
                _ => None,
            })
            .collect();
        let mut children = Vec::new();
        while let Some(g) = stack.pop() {
            let slot = &mut self.slots[g.0 as usize];
            if slot.marked {
                continue;
            }
            slot.marked = true;
            if let Some(obj) = &slot.obj {
                obj.trace(&mut children);
                stack.append(&mut children);
            }
        }
        let mut live_weight = 0;
        let mut live = 0;
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if slot.marked {
                slot.marked = false;
                live += 1;
                live_weight += slot.obj.as_ref().map_or(0, |o| o.weight());
            } else if slot.obj.is_some() {
                slot.obj = None;
                self.free.push(i as u32);
            }
        }
        self.live = live;
        self.allocated = 0;
        self.threshold = (live_weight * 2).max(MIN_THRESHOLD);
        self.collections += 1;
    }

    pub fn str(&self, v: Value) -> &str {
        match self.get(v.as_obj()) {
            Object::Str(s) => s,
            other => panic!("expected a string, found {other:?}"),
        }
    }

    pub fn array(&self, v: Value) -> &Vec<Value> {
        match self.get(v.as_obj()) {
            Object::Array(a) => a,
            other => panic!("expected an array, found {other:?}"),
        }
    }

    pub fn array_mut(&mut self, v: Value) -> &mut Vec<Value> {
        match self.get_mut(v.as_obj()) {
            Object::Array(a) => a,
            other => panic!("expected an array, found {other:?}"),
        }
    }

    pub fn map(&self, v: Value) -> &OrderedMap {
        match self.get(v.as_obj()) {
            Object::Map(m) => m,
            other => panic!("expected a map, found {other:?}"),
        }
    }

    pub fn map_mut(&mut self, v: Value) -> &mut OrderedMap {
        match self.get_mut(v.as_obj()) {
            Object::Map(m) => m,
            other => panic!("expected a map, found {other:?}"),
        }
    }

    pub fn new_str(&mut self, s: impl Into<String>) -> Value {
        Value::Obj(self.alloc(Object::Str(s.into())))
    }

    pub fn new_array(&mut self, v: Vec<Value>) -> Value {
        Value::Obj(self.alloc(Object::Array(v)))
    }

    pub fn new_tuple(&mut self, v: Vec<Value>) -> Value {
        Value::Obj(self.alloc(Object::Tuple(v.into_boxed_slice())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_unreachable_and_keeps_reachable() {
        let mut h = Heap::new();
        let s = h.new_str("kept");
        let inner = h.new_str("inner");
        let arr = h.new_array(vec![inner]);
        let garbage = h.new_str("garbage");
        let cyc = h.new_array(vec![]);
        if let Value::Obj(g) = cyc {
            h.array_mut(cyc).push(Value::Obj(g)); // a cycle
        }
        assert_eq!(h.live_objects(), 5);
        h.collect([s, arr].into_iter());
        assert_eq!(h.live_objects(), 3);
        assert_eq!(h.str(s), "kept");
        assert_eq!(h.str(h.array(arr)[0]), "inner");
        let _ = garbage;
        // Freed slots are reused.
        let again = h.new_str("new");
        assert!(matches!(again, Value::Obj(Gc(i)) if i == 3 || i == 4));
    }
}
