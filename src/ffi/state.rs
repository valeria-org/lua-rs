//! FFI state attached to the VM: the C type state plus the `miscmap` table of lj_ctype.h
//! (`-CTypeID` → metatable set by `ffi.metatype`, callback slot → Lua function, `""` → the
//! metatable of callback function pointers) and the clib metatable. Also creates the weak-keyed
//! finalizer table (`lj_ctype_initfin`, `GCROOT_FFI_FIN`).
//!
//! The state is shared through an `Rc` so that code holding a borrow of the C type table never
//! borrows the `Vm`; borrows of [`FfiState::cts`] are never held across calls into Lua.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::bc::MM;
use crate::gc::{Gc, Marker, Trace};
use crate::state::Vm;
use crate::table::Table;
use crate::value::Value;

use super::ctype::CTState;

/// The FFI state (`CTState` + `miscmap`).
pub struct FfiState {
    /// The C type table and callback state.
    pub cts: RefCell<CTState>,
    /// `cts->miscmap`.
    pub miscmap: Gc<Table>,
    /// Metatable of C library namespaces (environment of `ffi.load` upstream).
    pub clib_mt: Cell<Option<Gc<Table>>>,
}

/// Root object stored in `Vm::ffi`.
struct FfiRoot(Rc<FfiState>);

impl Trace for FfiRoot {
    fn trace(&self, m: &mut Marker) {
        m.mark(self.0.miscmap);
        if let Some(t) = self.0.clib_mt.get() {
            m.mark(t);
        }
    }
    fn size(&self) -> usize {
        0
    }
}

/// The FFI state of `vm`, if the FFI has been initialized.
pub fn try_state(vm: &Vm) -> Option<Rc<FfiState>> {
    let ext = vm.ffi.as_deref()?;
    let any: &dyn Any = ext;
    any.downcast_ref::<FfiRoot>().map(|r| r.0.clone())
}

/// `ctype_cts`: the FFI state of `vm`, created on first use (`lj_ctype_init` plus the
/// finalizer table of `lj_ctype_initfin`).
pub fn state(vm: &mut Vm) -> Rc<FfiState> {
    if let Some(s) = try_state(vm) {
        return s;
    }
    if vm.ffi_fin.is_none() {
        let t = vm.new_table_ah(0, 1).expect("small table");
        let k = vm.mm_name(MM::Mode);
        let kv = vm.new_str(b"k");
        t.set_str(k, Value::Str(kv)).expect("string key");
        t.set_metatable(Some(t));
        t.set_nomm(!(1u8 << MM::Mode as u8));
        vm.ffi_fin = Some(t);
    }
    let miscmap = vm.new_table_ah(0, 1).expect("small table");
    let st = Rc::new(FfiState {
        cts: RefCell::new(CTState::new()),
        miscmap,
        clib_mt: Cell::new(None),
    });
    let handler: super::callback::CallbackHandler = Rc::new(super::libffi::callback_handler);
    st.cts.borrow().cb.set_fallback(Some(handler));
    vm.ffi = Some(Box::new(FfiRoot(st.clone())));
    st
}

thread_local! {
    /// The VM running the innermost C call on this thread (`cts->L`): callbacks entered from C
    /// run on it.
    static CUR_VM: Cell<*mut Vm> = const { Cell::new(std::ptr::null_mut()) };
}

/// Make `vm` the VM that callbacks on this thread enter until the returned guard is dropped.
pub(crate) fn enter_vm(vm: *mut Vm) -> VmGuard {
    VmGuard(CUR_VM.with(|c| c.replace(vm)))
}

/// Restores the previous callback VM on drop.
pub(crate) struct VmGuard(*mut Vm);

impl Drop for VmGuard {
    fn drop(&mut self) {
        CUR_VM.with(|c| c.set(self.0));
    }
}

/// The VM callbacks enter (null outside of C calls).
pub(crate) fn current_vm() -> *mut Vm {
    CUR_VM.with(|c| c.get())
}
