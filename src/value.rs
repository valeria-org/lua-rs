//! Tagged Lua value (`TValue` in lj_obj.h, without NaN-boxing).

use crate::cdata::CData;
use crate::func::{Function, Proto};
use crate::gc::Gc;
use crate::lstr::LuaStr;
use crate::table::Table;
use crate::thread::Thread;
use crate::udata::Userdata;

/// A Lua value. 16 bytes, `Copy`.
///
/// `PartialEq` is Lua raw equality (`lj_obj_equal`): numbers compare with `==` (so NaN ≠ NaN,
/// -0 == 0), everything else by identity. Interned strings make identity equal to content
/// equality.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum Value {
    #[default]
    Nil,
    Bool(bool),
    Num(f64),
    Str(Gc<LuaStr>),
    Table(Gc<Table>),
    Func(Gc<Function>),
    Thread(Gc<Thread>),
    Udata(Gc<Userdata>),
    LightUd(usize),
    CData(Gc<CData>),
    /// Function prototype (`LJ_TPROTO`); only reachable through `jit.util.funck`/`funcinfo`.
    Proto(Gc<Proto>),
}

const _: () = assert!(std::mem::size_of::<Value>() == 16);

/// Basic type of a value (one entry per distinct `type()` result, light userdata separate).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[repr(u8)]
pub enum VType {
    Nil,
    Bool,
    Num,
    Str,
    Table,
    Func,
    Thread,
    Udata,
    LightUd,
    CData,
    Proto,
}

impl VType {
    pub const COUNT: usize = 11;

    /// Result of Lua `type()` (`lj_obj_typename`). Light userdata is `"userdata"`.
    pub fn name(self) -> &'static str {
        match self {
            VType::Nil => "nil",
            VType::Bool => "boolean",
            VType::Num => "number",
            VType::Str => "string",
            VType::Table => "table",
            VType::Func => "function",
            VType::Thread => "thread",
            VType::Udata | VType::LightUd => "userdata",
            VType::CData => "cdata",
            VType::Proto => "proto",
        }
    }
}

/// Exact conversion of a number to `int32_t` (`lj_num2int_check`): `Some(k)` iff `n == k as f64`.
/// `-0.0` converts to `0`.
#[inline]
pub fn num2int_check(n: f64) -> Option<i32> {
    let k = n as i32;
    if k as f64 == n { Some(k) } else { None }
}

impl Value {
    #[inline]
    pub fn vtype(self) -> VType {
        match self {
            Value::Nil => VType::Nil,
            Value::Bool(_) => VType::Bool,
            Value::Num(_) => VType::Num,
            Value::Str(_) => VType::Str,
            Value::Table(_) => VType::Table,
            Value::Func(_) => VType::Func,
            Value::Thread(_) => VType::Thread,
            Value::Udata(_) => VType::Udata,
            Value::LightUd(_) => VType::LightUd,
            Value::CData(_) => VType::CData,
            Value::Proto(_) => VType::Proto,
        }
    }

    /// Result of Lua `type()`.
    #[inline]
    pub fn type_name(self) -> &'static str {
        self.vtype().name()
    }

    #[inline]
    pub fn is_nil(self) -> bool {
        matches!(self, Value::Nil)
    }

    /// `nil` and `false` are falsy; everything else is truthy.
    #[inline]
    pub fn is_falsy(self) -> bool {
        matches!(self, Value::Nil | Value::Bool(false))
    }

    #[inline]
    pub fn is_truthy(self) -> bool {
        !self.is_falsy()
    }

    /// Lua raw equality (`rawequal`).
    #[inline]
    pub fn raw_equal(self, other: Value) -> bool {
        self == other
    }

    /// True for values referencing a GC object.
    #[inline]
    pub fn is_gc(self) -> bool {
        !matches!(self, Value::Nil | Value::Bool(_) | Value::Num(_) | Value::LightUd(_))
    }

    /// Canonical form of a table key: `-0` becomes `+0` (upstream `lj_tab_newkey`). Integer-valued
    /// floats need no further conversion since numbers are always `f64`.
    #[inline]
    pub fn normalize_key(self) -> Value {
        match self {
            // Float patterns compare with `==`, so this also matches -0.0.
            Value::Num(0.0) => Value::Num(0.0),
            v => v,
        }
    }

    /// Identity address of a GC object or light userdata (as printed by `tostring`); `None` for
    /// nil/boolean/number.
    pub fn gc_addr(self) -> Option<usize> {
        Some(match self {
            Value::Str(s) => s.addr(),
            Value::Table(t) => t.addr(),
            Value::Func(f) => f.addr(),
            Value::Thread(t) => t.addr(),
            Value::Udata(u) => u.addr(),
            Value::CData(c) => c.addr(),
            Value::Proto(p) => p.addr(),
            Value::LightUd(p) => p,
            Value::Nil | Value::Bool(_) | Value::Num(_) => return None,
        })
    }

    #[inline]
    pub fn as_num(self) -> Option<f64> {
        if let Value::Num(n) = self { Some(n) } else { None }
    }
    #[inline]
    pub fn as_bool(self) -> Option<bool> {
        if let Value::Bool(b) = self { Some(b) } else { None }
    }
    #[inline]
    pub fn as_str(self) -> Option<Gc<LuaStr>> {
        if let Value::Str(s) = self { Some(s) } else { None }
    }
    #[inline]
    pub fn as_table(self) -> Option<Gc<Table>> {
        if let Value::Table(t) = self { Some(t) } else { None }
    }
    #[inline]
    pub fn as_func(self) -> Option<Gc<Function>> {
        if let Value::Func(f) = self { Some(f) } else { None }
    }
    #[inline]
    pub fn as_thread(self) -> Option<Gc<Thread>> {
        if let Value::Thread(t) = self { Some(t) } else { None }
    }
    #[inline]
    pub fn as_udata(self) -> Option<Gc<Userdata>> {
        if let Value::Udata(u) = self { Some(u) } else { None }
    }
    #[inline]
    pub fn as_cdata(self) -> Option<Gc<CData>> {
        if let Value::CData(c) = self { Some(c) } else { None }
    }
    #[inline]
    pub fn as_lightud(self) -> Option<usize> {
        if let Value::LightUd(p) = self { Some(p) } else { None }
    }
}

impl From<bool> for Value {
    #[inline]
    fn from(b: bool) -> Value {
        Value::Bool(b)
    }
}
impl From<f64> for Value {
    #[inline]
    fn from(n: f64) -> Value {
        Value::Num(n)
    }
}
impl From<i32> for Value {
    #[inline]
    fn from(n: i32) -> Value {
        Value::Num(n as f64)
    }
}
impl From<Gc<LuaStr>> for Value {
    #[inline]
    fn from(s: Gc<LuaStr>) -> Value {
        Value::Str(s)
    }
}
impl From<Gc<Table>> for Value {
    #[inline]
    fn from(t: Gc<Table>) -> Value {
        Value::Table(t)
    }
}
impl From<Gc<Function>> for Value {
    #[inline]
    fn from(f: Gc<Function>) -> Value {
        Value::Func(f)
    }
}
impl From<Gc<Thread>> for Value {
    #[inline]
    fn from(t: Gc<Thread>) -> Value {
        Value::Thread(t)
    }
}
impl From<Gc<Userdata>> for Value {
    #[inline]
    fn from(u: Gc<Userdata>) -> Value {
        Value::Udata(u)
    }
}
impl From<Gc<CData>> for Value {
    #[inline]
    fn from(c: Gc<CData>) -> Value {
        Value::CData(c)
    }
}
impl<T: Into<Value>> From<Option<T>> for Value {
    #[inline]
    fn from(o: Option<T>) -> Value {
        o.map_or(Value::Nil, Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_and_names() {
        assert_eq!(std::mem::size_of::<Value>(), 16);
        assert_eq!(Value::Nil.type_name(), "nil");
        assert_eq!(Value::Bool(true).type_name(), "boolean");
        assert_eq!(Value::LightUd(0).type_name(), "userdata");
        assert!(Value::Bool(false).is_falsy() && Value::Nil.is_falsy());
        assert!(Value::Num(0.0).is_truthy());
    }

    #[test]
    fn raw_equality_and_keys() {
        assert_eq!(Value::Num(-0.0), Value::Num(0.0));
        assert_ne!(Value::Num(f64::NAN), Value::Num(f64::NAN));
        assert!(Value::Num(-0.0).normalize_key().as_num().unwrap().is_sign_positive());
        assert_eq!(num2int_check(-0.0), Some(0));
        assert_eq!(num2int_check(-2147483648.0), Some(i32::MIN));
        assert_eq!(num2int_check(2147483648.0), None);
        assert_eq!(num2int_check(1.5), None);
        assert_eq!(num2int_check(f64::NAN), None);
    }
}
