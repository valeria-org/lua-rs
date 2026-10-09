//! VM events. Port of lj_vmevent.c/h and the default `ERRFIN` handler of lib_aux.c
//! (`luaL_newstate`'s `error_finalizer`).
//!
//! Handlers live in `registry._VMEVENTS[hash]` where `hash = (h << 3) as i32` for the event
//! name hash `h` (`VMEVENT_HASHIDX`), as set by `jit.attach`.

use std::io::Write;

use crate::state::Vm;
use crate::thread::{ErrFunc, FrameKind};
use crate::value::Value;

/// Registry key of the VM event table (`LJ_VMEVENTS_REGKEY`).
pub const VMEVENTS_REGKEY: &str = "_VMEVENTS";

/// Table index of the `ERRFIN` (error in finalizer) event (`VMEVENT_HASH(LJ_VMEVENT_ERRFIN)`).
pub const VMEVENT_ERRFIN: i32 = (0x12d9_3888u32 << 3) as i32;

/// Table index for an event name, as computed by `jit.attach` (`VMEVENT_HASHIDX`).
pub fn vmevent_hash(name: &[u8]) -> i32 {
    let mut h = name.len() as u32;
    for &c in crate::parse::lex::cstr(name) {
        h ^= h.rotate_left(6).wrapping_add(c as u32);
    }
    (h << 3) as i32
}

/// `error_finalizer` (lib_aux.c).
fn error_finalizer(vm: &mut Vm) -> crate::error::LuaResult<usize> {
    let v = vm.arg(vm.nargs());
    let msg = vm.tostring_opt(v).unwrap_or_else(|| b"?".to_vec());
    let mut e = std::io::stderr();
    let _ = e.write_all(b"ERROR in finalizer: ");
    let _ = e.write_all(&msg);
    let _ = e.write_all(b"\n");
    let _ = e.flush();
    Ok(0)
}

pub(crate) fn install_default_handlers(vm: &mut Vm) {
    let key = vm.new_str(VMEVENTS_REGKEY.as_bytes());
    let t = vm.new_table(0, 2).expect("small table");
    let f = vm.new_native(crate::func::NativeImpl::Rust(error_finalizer), crate::func::FF_C, vec![], vm.globals);
    let _ = t.set_int(VMEVENT_ERRFIN, Value::Func(f));
    let _ = vm.registry.set_str(key, Value::Table(t));
}

impl Vm {
    /// `lj_vmevent_send(L, ERRFIN, ...)`: report an error raised by a finalizer.
    pub(crate) fn vmevent_errfin(&mut self, err: Value) {
        let key = self.new_str(VMEVENTS_REGKEY.as_bytes());
        let Value::Table(t) = self.registry.get_str(key) else { return };
        let f = t.get_int(VMEVENT_ERRFIN);
        if !matches!(f, Value::Func(_)) {
            return;
        }
        let cur = self.cur_thread;
        let th = self.vm_thread();
        self.cur_thread = th;
        let top = th.top();
        if th.max_stack().saturating_sub(top) < crate::thread::LJ_STACK_MIN && self.grow_stack(th, crate::thread::LJ_STACK_MIN).is_err() {
            self.cur_thread = cur;
            return;
        }
        th.set(top, f);
        th.set(top + 1, Value::Nil);
        th.set(top + 2, err);
        th.set_top(top + 3);
        let oldh = self.rt.hook.active;
        self.rt.hook.active = true;
        let r = self.call_at(th, top, 1, FrameKind::Cp(ErrFunc::None), 0);
        self.rt.hook.active = oldh;
        th.set_top(top);
        if let Err(e) = r {
            self.rt.thrown = None;
            let msg = match e.value {
                Value::Str(s) => s.as_bytes().to_vec(),
                _ => b"?".to_vec(),
            };
            let mut o = std::io::stderr();
            let _ = o.write_all(b"VM handler failed: ");
            let _ = o.write_all(&msg);
            let _ = o.write_all(b"\n");
        }
        self.cur_thread = cur;
    }
}
