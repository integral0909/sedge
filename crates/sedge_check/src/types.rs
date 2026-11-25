//! Types and unification.

use std::collections::HashMap;
use std::fmt;

pub type AdtId = u32;
pub type ParamId = u32;
pub type VarId = u32;

#[derive(Clone, Debug, PartialEq)]
pub enum Ty {
    Int,
    Float,
    Bool,
    Str,
    Unit,
    /// The type of expressions that never produce a value (`return`,
    /// `panic(...)`). It is compatible with every type.
    Never,
    Range,
    Array(Box<Ty>),
    Map(Box<Ty>, Box<Ty>),
    Tuple(Vec<Ty>),
    Fn(Vec<Ty>, Box<Ty>),
    Adt(AdtId, Vec<Ty>),
    /// A type parameter of the enclosing generic declaration.
    Param(ParamId),
    /// An inference variable.
    Var(VarId),
    /// Stands in after an error so it is not reported twice.
    Error,
}

/// What a method can be attached to with `impl`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TypeCon {
    Int,
    Float,
    Bool,
    Str,
    Array,
    Map,
    Adt(AdtId),
}

impl Ty {
    pub fn array(t: Ty) -> Ty {
        Ty::Array(Box::new(t))
    }

    pub fn func(params: Vec<Ty>, ret: Ty) -> Ty {
        Ty::Fn(params, Box::new(ret))
    }

    pub fn con(&self) -> Option<TypeCon> {
        Some(match self {
            Ty::Int => TypeCon::Int,
            Ty::Float => TypeCon::Float,
            Ty::Bool => TypeCon::Bool,
            Ty::Str => TypeCon::Str,
            Ty::Array(_) => TypeCon::Array,
            Ty::Map(..) => TypeCon::Map,
            Ty::Adt(id, _) => TypeCon::Adt(*id),
            _ => return None,
        })
    }

    pub fn is_error(&self) -> bool {
        matches!(self, Ty::Error)
    }

    pub fn has_error(&self) -> bool {
        let mut found = false;
        self.visit(&mut |t| found |= matches!(t, Ty::Error));
        found
    }

    pub fn visit(&self, f: &mut impl FnMut(&Ty)) {
        f(self);
        match self {
            Ty::Array(t) => t.visit(f),
            Ty::Map(k, v) => {
                k.visit(f);
                v.visit(f);
            }
            Ty::Tuple(ts) | Ty::Adt(_, ts) => ts.iter().for_each(|t| t.visit(f)),
            Ty::Fn(ps, r) => {
                ps.iter().for_each(|t| t.visit(f));
                r.visit(f);
            }
            _ => {}
        }
    }

    /// Replaces type parameters using `map`.
    pub fn subst(&self, map: &HashMap<ParamId, Ty>) -> Ty {
        match self {
            Ty::Param(p) => map.get(p).cloned().unwrap_or(Ty::Param(*p)),
            Ty::Array(t) => Ty::array(t.subst(map)),
            Ty::Map(k, v) => Ty::Map(Box::new(k.subst(map)), Box::new(v.subst(map))),
            Ty::Tuple(ts) => Ty::Tuple(ts.iter().map(|t| t.subst(map)).collect()),
            Ty::Adt(id, ts) => Ty::Adt(*id, ts.iter().map(|t| t.subst(map)).collect()),
            Ty::Fn(ps, r) => Ty::Fn(ps.iter().map(|t| t.subst(map)).collect(), Box::new(r.subst(map))),
            other => other.clone(),
        }
    }
}

/// Names needed to print types.
pub trait TypeNames {
    fn adt_name(&self, id: AdtId) -> &str;
    fn param_name(&self, id: ParamId) -> &str;
}

pub struct Display<'a, N: TypeNames> {
    pub ty: &'a Ty,
    pub names: &'a N,
}

impl<N: TypeNames> fmt::Display for Display<'_, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_ty(f, self.ty, self.names)
    }
}

fn write_list<N: TypeNames>(f: &mut fmt::Formatter<'_>, ts: &[Ty], names: &N) -> fmt::Result {
    for (i, t) in ts.iter().enumerate() {
        if i > 0 {
            f.write_str(", ")?;
        }
        write_ty(f, t, names)?;
    }
    Ok(())
}

fn write_ty<N: TypeNames>(f: &mut fmt::Formatter<'_>, ty: &Ty, names: &N) -> fmt::Result {
    match ty {
        Ty::Int => f.write_str("Int"),
        Ty::Float => f.write_str("Float"),
        Ty::Bool => f.write_str("Bool"),
        Ty::Str => f.write_str("String"),
        Ty::Unit => f.write_str("()"),
        Ty::Never => f.write_str("Never"),
        Ty::Range => f.write_str("Range"),
        Ty::Error => f.write_str("{error}"),
        Ty::Var(_) => f.write_str("_"),
        Ty::Param(p) => f.write_str(names.param_name(*p)),
        Ty::Array(t) => {
            f.write_str("[")?;
            write_ty(f, t, names)?;
            f.write_str("]")
        }
        Ty::Map(k, v) => {
            f.write_str("Map[")?;
            write_ty(f, k, names)?;
            f.write_str(", ")?;
            write_ty(f, v, names)?;
            f.write_str("]")
        }
        Ty::Tuple(ts) => {
            f.write_str("(")?;
            write_list(f, ts, names)?;
            if ts.len() == 1 {
                f.write_str(",")?;
            }
            f.write_str(")")
        }
        Ty::Fn(ps, r) => {
            f.write_str("fn(")?;
            write_list(f, ps, names)?;
            f.write_str(")")?;
            if **r != Ty::Unit {
                f.write_str(" -> ")?;
                write_ty(f, r, names)?;
            }
            Ok(())
        }
        Ty::Adt(id, args) => {
            f.write_str(names.adt_name(*id))?;
            if !args.is_empty() {
                f.write_str("[")?;
                write_list(f, args, names)?;
                f.write_str("]")?;
            }
            Ok(())
        }
    }
}

/// Inference variables and their solutions.
#[derive(Default)]
pub struct Unifier {
    vars: Vec<Option<Ty>>,
}

#[derive(Debug)]
pub enum UnifyError {
    Mismatch,
    /// The variable would have to contain itself.
    Infinite,
}

impl Unifier {
    pub fn fresh(&mut self) -> Ty {
        self.vars.push(None);
        Ty::Var(self.vars.len() as u32 - 1)
    }

    /// Follows solved variables at the top level only.
    pub fn shallow(&self, ty: &Ty) -> Ty {
        let mut t = ty.clone();
        while let Ty::Var(v) = t {
            match &self.vars[v as usize] {
                Some(next) => t = next.clone(),
                None => return t,
            }
        }
        t
    }

    /// Replaces every solved variable, recursively.
    pub fn resolve(&self, ty: &Ty) -> Ty {
        match self.shallow(ty) {
            Ty::Array(t) => Ty::array(self.resolve(&t)),
            Ty::Map(k, v) => Ty::Map(Box::new(self.resolve(&k)), Box::new(self.resolve(&v))),
            Ty::Tuple(ts) => Ty::Tuple(ts.iter().map(|t| self.resolve(t)).collect()),
            Ty::Adt(id, ts) => Ty::Adt(id, ts.iter().map(|t| self.resolve(t)).collect()),
            Ty::Fn(ps, r) => Ty::Fn(
                ps.iter().map(|t| self.resolve(t)).collect(),
                Box::new(self.resolve(&r)),
            ),
            t => t,
        }
    }

    fn occurs(&self, v: VarId, ty: &Ty) -> bool {
        let mut found = false;
        self.resolve(ty).visit(&mut |t| found |= *t == Ty::Var(v));
        found
    }

    pub fn unify(&mut self, a: &Ty, b: &Ty) -> Result<(), UnifyError> {
        let a = self.shallow(a);
        let b = self.shallow(b);
        match (&a, &b) {
            (Ty::Error, _) | (_, Ty::Error) => Ok(()),
            (Ty::Never, _) | (_, Ty::Never) => Ok(()),
            (Ty::Var(x), Ty::Var(y)) if x == y => Ok(()),
            (Ty::Var(x), other) | (other, Ty::Var(x)) => {
                if self.occurs(*x, other) {
                    return Err(UnifyError::Infinite);
                }
                self.vars[*x as usize] = Some(other.clone());
                Ok(())
            }
            (Ty::Array(x), Ty::Array(y)) => self.unify(x, y),
            (Ty::Map(k1, v1), Ty::Map(k2, v2)) => {
                self.unify(k1, k2)?;
                self.unify(v1, v2)
            }
            (Ty::Tuple(xs), Ty::Tuple(ys)) if xs.len() == ys.len() => {
                for (x, y) in xs.iter().zip(ys) {
                    self.unify(x, y)?;
                }
                Ok(())
            }
            (Ty::Fn(p1, r1), Ty::Fn(p2, r2)) if p1.len() == p2.len() => {
                for (x, y) in p1.iter().zip(p2) {
                    self.unify(x, y)?;
                }
                self.unify(r1, r2)
            }
            (Ty::Adt(i, xs), Ty::Adt(j, ys)) if i == j => {
                for (x, y) in xs.iter().zip(ys) {
                    self.unify(x, y)?;
                }
                Ok(())
            }
            (x, y) if x == y => Ok(()),
            _ => Err(UnifyError::Mismatch),
        }
    }

    /// Tries a unification and rolls it back if it fails.
    pub fn try_unify(&mut self, a: &Ty, b: &Ty) -> bool {
        let saved = self.vars.clone();
        if self.unify(a, b).is_ok() {
            true
        } else {
            self.vars = saved;
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoNames;
    impl TypeNames for NoNames {
        fn adt_name(&self, _: AdtId) -> &str {
            "Option"
        }
        fn param_name(&self, _: ParamId) -> &str {
            "T"
        }
    }

    #[test]
    fn unify_and_resolve() {
        let mut u = Unifier::default();
        let a = u.fresh();
        let b = u.fresh();
        u.unify(&Ty::array(a.clone()), &Ty::array(b.clone())).unwrap();
        u.unify(&b, &Ty::Int).unwrap();
        assert_eq!(u.resolve(&a), Ty::Int);
        assert!(u.unify(&a, &Ty::Str).is_err());
    }

    #[test]
    fn occurs_check() {
        let mut u = Unifier::default();
        let a = u.fresh();
        assert!(matches!(
            u.unify(&a, &Ty::array(a.clone())),
            Err(UnifyError::Infinite)
        ));
    }

    #[test]
    fn try_unify_rolls_back() {
        let mut u = Unifier::default();
        let a = u.fresh();
        assert!(!u.try_unify(
            &Ty::Tuple(vec![a.clone(), Ty::Int]),
            &Ty::Tuple(vec![Ty::Str, Ty::Bool])
        ));
        assert_eq!(u.resolve(&a), a);
    }

    #[test]
    fn display() {
        let t = Ty::func(
            vec![Ty::array(Ty::Param(0)), Ty::Map(Box::new(Ty::Str), Box::new(Ty::Int))],
            Ty::Adt(0, vec![Ty::Tuple(vec![Ty::Int, Ty::Float])]),
        );
        let s = Display { ty: &t, names: &NoNames }.to_string();
        assert_eq!(s, "fn([T], Map[String, Int]) -> Option[(Int, Float)]");
    }
}
