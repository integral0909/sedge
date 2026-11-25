//! Exhaustiveness and reachability of patterns.
//!
//! This is the usefulness algorithm from Luc Maranget, "Warnings for
//! pattern matching" (2007). A pattern vector `q` is useful with respect to
//! a matrix of rows if some value matches `q` and no row. A `match` is
//! exhaustive when a row of wildcards is not useful, and an arm is
//! reachable when it is useful with respect to the arms above it. When a
//! wildcard row is useful, the algorithm also builds an example value that
//! no arm matches, which the error message shows.

use crate::typed::{AdtDef, AdtKind};
use crate::types::{AdtId, ParamId, Ty};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub enum Ctor {
    Variant(AdtId, u32),
    Struct(AdtId),
    Tuple(usize),
    Bool(bool),
    Int(i64),
    Str(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Pat {
    Wild,
    Ctor(Ctor, Vec<Pat>),
    Or(Vec<Pat>),
}

type Resolve<'a> = &'a dyn Fn(&Ty) -> Ty;

fn arity(adts: &[AdtDef], c: &Ctor) -> usize {
    match c {
        Ctor::Variant(a, v) => adts[*a as usize].variants()[*v as usize].fields.len(),
        Ctor::Struct(a) => adts[*a as usize].fields().len(),
        Ctor::Tuple(n) => *n,
        _ => 0,
    }
}

/// Types of a constructor's fields, given the type it constructs.
fn field_types(adts: &[AdtDef], resolve: Resolve, c: &Ctor, ty: &Ty) -> Vec<Ty> {
    let n = arity(adts, c);
    let args = match resolve(ty) {
        Ty::Adt(_, args) => args,
        Ty::Tuple(ts) if matches!(c, Ctor::Tuple(_)) => return ts,
        _ => return vec![Ty::Error; n],
    };
    let subst = |a: AdtId, t: &Ty| {
        let map: HashMap<ParamId, Ty> = adts[a as usize]
            .params
            .iter()
            .copied()
            .zip(args.iter().cloned())
            .collect();
        resolve(&t.subst(&map))
    };
    match c {
        Ctor::Variant(a, v) => adts[*a as usize].variants()[*v as usize]
            .fields
            .iter()
            .map(|t| subst(*a, t))
            .collect(),
        Ctor::Struct(a) => adts[*a as usize]
            .fields()
            .iter()
            .map(|f| subst(*a, &f.ty))
            .collect(),
        _ => vec![Ty::Error; n],
    }
}

/// Every constructor of a type, or `None` when there are infinitely many
/// (integers, strings) or the type is opaque.
fn all_ctors(adts: &[AdtDef], resolve: Resolve, ty: &Ty) -> Option<Vec<Ctor>> {
    match resolve(ty) {
        Ty::Bool => Some(vec![Ctor::Bool(false), Ctor::Bool(true)]),
        Ty::Unit => Some(vec![Ctor::Tuple(0)]),
        Ty::Tuple(ts) => Some(vec![Ctor::Tuple(ts.len())]),
        Ty::Adt(a, _) => match &adts[a as usize].kind {
            AdtKind::Enum(vs) => Some((0..vs.len() as u32).map(|v| Ctor::Variant(a, v)).collect()),
            AdtKind::Struct(_) => Some(vec![Ctor::Struct(a)]),
        },
        _ => None,
    }
}

/// Rows with or-patterns at the head expanded into one row per alternative.
fn expand_head(rows: &[Vec<Pat>]) -> Vec<Vec<Pat>> {
    let mut out = Vec::new();
    for row in rows {
        match row.first() {
            Some(Pat::Or(alts)) => {
                let mut sub = Vec::new();
                for alt in alts {
                    let mut r = vec![alt.clone()];
                    r.extend_from_slice(&row[1..]);
                    sub.push(r);
                }
                out.extend(expand_head(&sub));
            }
            _ => out.push(row.clone()),
        }
    }
    out
}

/// Rows that match constructor `c` at the head, with the head replaced by
/// its fields.
fn specialize(adts: &[AdtDef], rows: &[Vec<Pat>], c: &Ctor) -> Vec<Vec<Pat>> {
    let n = arity(adts, c);
    let mut out = Vec::new();
    for row in rows {
        match &row[0] {
            Pat::Wild => {
                let mut r = vec![Pat::Wild; n];
                r.extend_from_slice(&row[1..]);
                out.push(r);
            }
            Pat::Ctor(rc, args) if rc == c => {
                let mut r = args.clone();
                r.extend_from_slice(&row[1..]);
                out.push(r);
            }
            Pat::Ctor(..) => {}
            Pat::Or(_) => unreachable!("expanded"),
        }
    }
    out
}

/// Rows whose head is a wildcard, without the head.
fn default_rows(rows: &[Vec<Pat>]) -> Vec<Vec<Pat>> {
    rows.iter()
        .filter(|r| matches!(r[0], Pat::Wild))
        .map(|r| r[1..].to_vec())
        .collect()
}

/// If `q` is useful with respect to `rows`, returns a witness: values (as
/// patterns) that match `q` but no row.
pub fn useful(
    adts: &[AdtDef],
    resolve: Resolve,
    rows: &[Vec<Pat>],
    q: &[Pat],
    tys: &[Ty],
) -> Option<Vec<Pat>> {
    if q.is_empty() {
        return if rows.is_empty() { Some(Vec::new()) } else { None };
    }
    let rows = expand_head(rows);
    let ty = &tys[0];
    match &q[0] {
        Pat::Or(alts) => {
            for alt in alts {
                let mut q2 = vec![alt.clone()];
                q2.extend_from_slice(&q[1..]);
                if let Some(w) = useful(adts, resolve, &rows, &q2, tys) {
                    return Some(w);
                }
            }
            None
        }
        Pat::Ctor(c, args) => {
            let fts = field_types(adts, resolve, c, ty);
            let mut q2 = args.clone();
            q2.extend_from_slice(&q[1..]);
            let mut t2 = fts;
            t2.extend_from_slice(&tys[1..]);
            let w = useful(adts, resolve, &specialize(adts, &rows, c), &q2, &t2)?;
            let n = args.len();
            let mut out = vec![Pat::Ctor(c.clone(), w[..n].to_vec())];
            out.extend_from_slice(&w[n..]);
            Some(out)
        }
        Pat::Wild => {
            let used: Vec<Ctor> = {
                let mut v: Vec<Ctor> = Vec::new();
                for r in &rows {
                    if let Pat::Ctor(c, _) = &r[0] {
                        if !v.contains(c) {
                            v.push(c.clone());
                        }
                    }
                }
                v
            };
            let all = all_ctors(adts, resolve, ty);
            let complete = match &all {
                Some(all) => !all.is_empty() && all.iter().all(|c| used.contains(c)),
                None => false,
            };
            if complete {
                for c in all.unwrap() {
                    let n = arity(adts, &c);
                    let mut q2 = vec![Pat::Wild; n];
                    q2.extend_from_slice(&q[1..]);
                    let mut t2 = field_types(adts, resolve, &c, ty);
                    t2.extend_from_slice(&tys[1..]);
                    if let Some(w) = useful(adts, resolve, &specialize(adts, &rows, &c), &q2, &t2) {
                        let mut out = vec![Pat::Ctor(c.clone(), w[..n].to_vec())];
                        out.extend_from_slice(&w[n..]);
                        return Some(out);
                    }
                }
                None
            } else {
                let w = useful(adts, resolve, &default_rows(&rows), &q[1..], &tys[1..])?;
                // Name a constructor no row uses, when there is one.
                let head = match &all {
                    Some(all) if !used.is_empty() => match all.iter().find(|c| !used.contains(c)) {
                        Some(c) => Pat::Ctor(c.clone(), vec![Pat::Wild; arity(adts, c)]),
                        None => Pat::Wild,
                    },
                    _ => Pat::Wild,
                };
                let mut out = vec![head];
                out.extend(w);
                Some(out)
            }
        }
    }
}

/// A value of type `ty` that no row matches, if there is one.
pub fn missing(adts: &[AdtDef], resolve: Resolve, rows: &[Vec<Pat>], ty: &Ty) -> Option<Pat> {
    useful(adts, resolve, rows, &[Pat::Wild], &[ty.clone()]).map(|mut w| w.remove(0))
}

/// Shows a witness the way it would be written in a pattern.
pub fn show(adts: &[AdtDef], p: &Pat) -> String {
    match p {
        Pat::Wild => "_".to_string(),
        Pat::Or(alts) => alts.iter().map(|a| show(adts, a)).collect::<Vec<_>>().join(" | "),
        Pat::Ctor(c, args) => match c {
            Ctor::Bool(b) => b.to_string(),
            Ctor::Int(i) => i.to_string(),
            Ctor::Str(s) => format!("{s:?}"),
            Ctor::Tuple(_) => {
                let inner: Vec<String> = args.iter().map(|a| show(adts, a)).collect();
                if inner.len() == 1 {
                    format!("({},)", inner[0])
                } else {
                    format!("({})", inner.join(", "))
                }
            }
            Ctor::Variant(a, v) => {
                let name = &adts[*a as usize].variants()[*v as usize].name;
                if args.is_empty() {
                    name.clone()
                } else {
                    let inner: Vec<String> = args.iter().map(|a| show(adts, a)).collect();
                    format!("{name}({})", inner.join(", "))
                }
            }
            Ctor::Struct(a) => {
                let def = &adts[*a as usize];
                let shown: Vec<String> = def
                    .fields()
                    .iter()
                    .zip(args)
                    .filter(|(_, p)| !matches!(p, Pat::Wild))
                    .map(|(f, p)| format!("{}: {}", f.name, show(adts, p)))
                    .collect();
                if shown.is_empty() {
                    format!("{} {{ .. }}", def.name)
                } else {
                    format!("{} {{ {}, .. }}", def.name, shown.join(", "))
                }
            }
        },
    }
}
