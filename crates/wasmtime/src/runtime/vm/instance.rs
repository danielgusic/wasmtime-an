//! An `Instance` contains all the runtime state used by execution of a
//! wasm module (except its callstack and register state). An
//! `InstanceHandle` is a reference-counting handle for an `Instance`.

use crate::code::ModuleWithCode;
use crate::module::ModuleRegistry;
use crate::runtime::vm::export::{Export, ExportMemory};
use crate::runtime::vm::memory::{Memory, RuntimeMemoryCreator};
use crate::runtime::vm::table::{Table, TableElementType};
use crate::runtime::vm::vmcontext::{
    VMBuiltinFunctionsArray, VMContext, VMFuncRef, VMFunctionImport, VMGlobalDefinition,
    VMGlobalImport, VMMemoryDefinition, VMMemoryImport, VMOpaqueContext, VMStoreContext,
    VMTableDefinition, VMTableImport, VMTagDefinition, VMTagImport,
};
use crate::runtime::vm::{
    GcStore, HostResult, Imports, ModuleRuntimeInfo, SendSyncPtr, VMGcRef, VMGlobalKind, VMStore,
    VMStoreRawPtr, VmPtr, VmSafe, WasmFault, catch_unwind_and_record_trap,
};
use crate::store::{
    AutoAssertNoGc, InstanceId, StoreId, StoreInstanceId, StoreOpaque, StoreResourceLimiter,
};
use crate::vm::{VMWasmCallFunction, ValRaw};
use crate::{OpaqueRootScope, Val};
use crate::{ValType, prelude::*};
use alloc::sync::Arc;
use core::alloc::Layout;
use core::marker;
use core::ops::Range;
use core::pin::Pin;
use core::ptr::NonNull;
#[cfg(target_has_atomic = "64")]
use core::sync::atomic::AtomicU64;
use core::{mem, ptr};
#[cfg(feature = "gc")]
use wasmtime_environ::ModuleInternedTypeIndex;
use wasmtime_environ::error::OutOfMemory;
use wasmtime_environ::{
    DataIndex, DefinedGlobalIndex, DefinedMemoryIndex, DefinedTableIndex, DefinedTagIndex,
    ElemIndex, EntityIndex, EntityRef, FuncIndex, GlobalIndex, HostPtr, MemoryIndex, PtrSize,
    TableIndex, TableInitialValue, TagIndex, Trap, VMCONTEXT_MAGIC, VMOffsets, VMSharedTypeIndex,
    WasmRefType, packed_option::ReservedValue,
};
#[cfg(feature = "wmemcheck")]
use wasmtime_wmemcheck::Wmemcheck;

mod allocator;
pub use allocator::*;

/// A type that roughly corresponds to a WebAssembly instance, but is also used
/// for host-defined objects.
///
/// Instances here can correspond to actual instantiated modules, but it's also
/// used ubiquitously for host-defined objects. For example creating a
/// host-defined memory will have a `module` that looks like it exports a single
/// memory (and similar for other constructs).
///
/// This `Instance` type is used as a ubiquitous representation for WebAssembly
/// values, whether or not they were created on the host or through a module.
///
/// # Ownership
///
/// This structure is never allocated directly but is instead managed through
/// an `InstanceHandle`. This structure ends with a `VMContext` which has a
/// dynamic size corresponding to the `module` configured within. Memory
/// management of this structure is always done through `InstanceHandle` as the
/// sole owner of an instance.
///
/// # `Instance` and `Pin`
///
/// Given an instance it is accompanied with trailing memory for the
/// appropriate `VMContext`. The `Instance` also holds `runtime_info` and other
/// information pointing to relevant offsets for the `VMContext`. Thus it is
/// not sound to mutate `runtime_info` after an instance is created. More
/// generally it's also not safe to "swap" instances, for example given two
/// `&mut Instance` values it's not sound to swap them as then the `VMContext`
/// values are inaccurately described.
///
/// To encapsulate this guarantee this type is only ever mutated through Rust's
/// `Pin` type. All mutable methods here take `self: Pin<&mut Self>` which
/// statically disallows safe access to `&mut Instance`. There are assorted
/// "projection methods" to go from `Pin<&mut Instance>` to `&mut T` for
/// individual fields, for example `memories_mut`. More methods can be added as
/// necessary or methods may also be added to project multiple fields at a time
/// if necessary to. The precise ergonomics around getting mutable access to
/// some fields (but notably not `runtime_info`) is probably going to evolve
/// over time.
///
/// Note that is is not sound to basically ever pass around `&mut Instance`.
/// That should always instead be `Pin<&mut Instance>`. All usage of
/// `Pin::new_unchecked` should be here in this module in just a few `unsafe`
/// locations and it's recommended to use existing helpers if you can.
#[repr(C)] // ensure that the vmctx field is last.
pub struct Instance {
    /// The index, within a `Store` that this instance lives at
    id: InstanceId,

    /// The runtime info (corresponding to the "compiled module"
    /// abstraction in higher layers) that is retained and needed for
    /// lazy initialization. This provides access to the underlying
    /// Wasm module entities, the compiled JIT code, metadata about
    /// functions, lazy initialization state, etc.
    //
    // SAFETY: this field cannot be overwritten after an instance is created. It
    // must contain this exact same value for the entire lifetime of this
    // instance. This enables borrowing the info's `Module` and this instance at
    // the same time (instance mutably, module not). Additionally it enables
    // borrowing a store mutably at the same time as a contained instance.
    runtime_info: ModuleRuntimeInfo,

    /// WebAssembly linear memory data.
    ///
    /// This is where all runtime information about defined linear memories in
    /// this module lives.
    ///
    /// The `MemoryAllocationIndex` was given from our `InstanceAllocator` and
    /// must be given back to the instance allocator when deallocating each
    /// memory.
    memories: TryPrimaryMap<DefinedMemoryIndex, (MemoryAllocationIndex, Memory)>,

    /// AN-encoding shadow buffers, parallel to [`Self::memories`].
    ///
    /// Each entry is either `None` (no shadow allocated for this memory; this
    /// is the case when AN-encoding is disabled, when the memory is shared,
    /// or before [`Self::set_an_enc_shadows`] has run) or a `Box<[u8]>` of
    /// size `ENC_MEM_GROWTH_FACTOR * raw_byte_size`. JIT code reads the base
    /// pointer of each shadow from the corresponding `VMContext` slot at
    /// `VMOffsets::vmctx_an_enc_memory_base(idx)`; this map is the Rust-side
    /// owner that keeps those buffers alive for the lifetime of the
    /// instance. See `wasmtime_environ::ENC_MEM_GROWTH_FACTOR` for the layout
    /// invariant.
    an_enc_shadows: TryPrimaryMap<DefinedMemoryIndex, Option<Box<[u8]>>>,

    /// AN-encoding "whole memory dirty" flags, parallel to
    /// [`Self::an_enc_shadows`].
    ///
    /// An entry is `true` when the host has obtained an untracked
    /// whole-memory mutable view of the raw memory (e.g. via
    /// `Memory::data_mut` / `Memory::data_and_store_mut`): the host may
    /// write anywhere through that borrow, so the shadow can no longer be
    /// assumed to mirror raw bytes. The flag is consumed (read + cleared)
    /// by the `an_resync_host_boundary` libcall, which re-encodes the whole
    /// memory from raw before wasm resumes. Host write paths that know
    /// their exact byte range instead re-encode immediately via
    /// [`Self::an_encode_range_from_raw`] and never set this flag.
    an_whole_dirty: TryPrimaryMap<DefinedMemoryIndex, bool>,

    /// WebAssembly table data.
    ///
    /// Like memories, this is only for defined tables in the module and
    /// contains all of their runtime state.
    ///
    /// The `TableAllocationIndex` was given from our `InstanceAllocator` and
    /// must be given back to the instance allocator when deallocating each
    /// table.
    tables: TryPrimaryMap<DefinedTableIndex, (TableAllocationIndex, Table)>,

    /// Evaluated passive element segments.
    ///
    /// If an entry is none, then it has been dropped.
    //
    // TODO(#12621): This should be a `TrySecondaryMap<PassiveElemIndex, _>`
    // but that type is currently footgun-y / isn't actually OOM-safe yet.
    passive_elements: TryVec<PassiveElementSegment>,

    /// Stores the dropped passive data segments in this instantiation by index.
    /// If the index is present in the set, the segment has been dropped.
    dropped_data: TryEntitySet<DataIndex>,

    // TODO: add support for multiple memories; `wmemcheck_state` corresponds to
    // memory 0.
    #[cfg(feature = "wmemcheck")]
    pub(crate) wmemcheck_state: Option<Wmemcheck>,

    /// Self-pointer back to `Store<T>` and its functions. Not present for
    /// the brief time that `Store<T>` is itself being created. Also not
    /// present for some niche uses that are disconnected from stores (e.g.
    /// cross-thread stuff used in `InstancePre`)
    store: Option<VMStoreRawPtr>,

    /// Additional context used by compiled wasm code. This field is last, and
    /// represents a dynamically-sized array that extends beyond the nominal
    /// end of the struct (similar to a flexible array member).
    vmctx: OwnedVMContext<VMContext>,
}

impl Instance {
    /// Create an instance at the given memory address.
    ///
    /// It is assumed the memory was properly aligned and the
    /// allocation was `alloc_size` in bytes.
    ///
    /// # Safety
    ///
    /// The `req.imports` field must be appropriately sized/typed for the module
    /// being allocated according to `req.runtime_info`. Additionally `memories`
    /// and `tables` must have been allocated for `req.store`.
    unsafe fn new(
        req: InstanceAllocationRequest,
        memories: TryPrimaryMap<DefinedMemoryIndex, (MemoryAllocationIndex, Memory)>,
        tables: TryPrimaryMap<DefinedTableIndex, (TableAllocationIndex, Table)>,
    ) -> Result<InstanceHandle, OutOfMemory> {
        let module = req.runtime_info.env_module();
        let memory_tys = &module.memories;
        let passive_elements = TryVec::with_capacity(module.passive_elements.len())?;
        let dropped_data = TryEntitySet::with_capacity(module.passive_data_map.len())?;
        // Pre-size the AN-encoding shadow map with `None` entries so that
        // `Self::set_an_enc_shadows` (called later from `set_store`) can index
        // by `DefinedMemoryIndex` and replace the entries in place. When
        // AN-encoding is disabled on the engine, the entries stay `None` and
        // no shadow is allocated.
        let mut an_enc_shadows = TryPrimaryMap::with_capacity(module.num_defined_memories())?;
        let mut an_whole_dirty = TryPrimaryMap::with_capacity(module.num_defined_memories())?;
        for _ in 0..module.num_defined_memories() {
            an_enc_shadows.push(None)?;
            an_whole_dirty.push(false)?;
        }

        #[cfg(feature = "wmemcheck")]
        let wmemcheck_state = if req.store.engine().config().wmemcheck {
            let size = memory_tys
                .iter()
                .next()
                .map(|memory| memory.1.limits.min)
                .unwrap_or(0)
                * 64
                * 1024;
            Some(Wmemcheck::new(size.try_into().unwrap()))
        } else {
            None
        };
        #[cfg(not(feature = "wmemcheck"))]
        let _ = memory_tys;

        let mut ret = OwnedInstance::new(Instance {
            id: req.id,
            runtime_info: req.runtime_info.clone(),
            memories,
            an_enc_shadows,
            an_whole_dirty,
            tables,
            passive_elements,
            dropped_data,
            #[cfg(feature = "wmemcheck")]
            wmemcheck_state,
            store: None,
            vmctx: OwnedVMContext::new(),
        })?;

        // SAFETY: this vmctx was allocated with the same layout above, so it
        // should be safe to initialize with the same values here.
        unsafe {
            ret.get_mut().initialize_vmctx(req.store, req.imports);
        }
        Ok(ret)
    }

    /// Trace element segment GC roots inside this `Instance`.
    ///
    /// # Safety
    ///
    /// This instance must live for the duration of the associated GC cycle.
    #[cfg(feature = "gc")]
    pub(crate) unsafe fn trace_element_segment_roots(
        self: Pin<&mut Self>,
        gc_roots: &mut crate::vm::GcRootsList,
    ) {
        for segment in self.passive_elements_mut().iter_mut() {
            if segment.needs_gc_rooting {
                for e in segment.elements() {
                    if e.get_vmgcref().is_none() {
                        continue;
                    }

                    let root: SendSyncPtr<ValRaw> = e.into();

                    // Safety: We know this is a type that needs GC rooting and
                    // the lifetime is implied by our safety contract.
                    unsafe {
                        gc_roots.add_val_raw_root(root, "passive element segment");
                    }
                }
            }
        }
    }

    /// Converts a raw `VMContext` pointer into a raw `Instance` pointer.
    ///
    /// # Safety
    ///
    /// Calling this function safely requires that `vmctx` is a valid allocation
    /// of a `VMContext` which is derived from `Instance::new`. To safely
    /// convert the returned raw pointer into a safe instance pointer callers
    /// will also want to uphold guarantees such as:
    ///
    /// * The instance should not be in use elsewhere. For example you can't
    ///   call this function twice, turn both raw pointers into safe pointers,
    ///   and then use both safe pointers.
    /// * There should be no other active mutable borrow to any other instance
    ///   within the same store. Note that this is not restricted to just this
    ///   instance pointer, but to all instances in a store. Instances can
    ///   safely traverse to other instances "laterally" meaning that a mutable
    ///   borrow on one is a mutable borrow on all.
    /// * There should be no active mutable borrow on the store accessible at
    ///   the same time the instance is turned. Instances are owned by a store
    ///   and a store can be used to acquire a safe instance borrow at any time.
    /// * The lifetime of the usage of the instance should not be unnecessarily
    ///   long, for example it cannot be `'static`.
    ///
    /// Other entrypoints exist for converting from a raw `VMContext` to a safe
    /// pointer such as:
    ///
    /// * `Instance::enter_host_from_wasm`
    /// * `Instance::sibling_vmctx{,_mut}`
    ///
    /// These place further restrictions on the API signature to satisfy some of
    /// the above points.
    #[inline]
    pub(crate) unsafe fn from_vmctx(vmctx: NonNull<VMContext>) -> NonNull<Instance> {
        // SAFETY: The validity of `byte_sub` relies on `vmctx` being a valid
        // allocation.
        unsafe {
            vmctx
                .byte_sub(mem::size_of::<Instance>())
                .cast::<Instance>()
        }
    }

    /// Encapsulated entrypoint to the host from WebAssembly, converting a raw
    /// `VMContext` pointer into a `VMStore` plus an `InstanceId`.
    ///
    /// This is an entrypoint for core wasm entering back into the host. This is
    /// used for both host functions and libcalls for example. This will execute
    /// the closure `f` with safer Internal types than a raw `VMContext`
    /// pointer.
    ///
    /// The closure `f` will have its errors caught, handled, and translated to
    /// an ABI-safe return value to give back to wasm. This includes both normal
    /// errors such as traps as well as panics.
    ///
    /// # Safety
    ///
    /// Callers must ensure that `vmctx` is a valid allocation and is safe to
    /// dereference at this time. That's generally only true when it's a
    /// wasm-provided value and this is the first function called after entering
    /// the host. Otherwise this could unsafely alias the store with a mutable
    /// pointer, for example.
    #[inline]
    pub(crate) unsafe fn enter_host_from_wasm<R>(
        vmctx: NonNull<VMContext>,
        f: impl FnOnce(&mut dyn VMStore, InstanceId) -> R,
    ) -> R::Abi
    where
        R: HostResult,
    {
        // SAFETY: It's a contract of this function that `vmctx` is a valid
        // pointer with neither the store nor other instances actively in use
        // when this is called, so it should be safe to acquire a mutable
        // pointer to the store and read the instance pointer.
        let (store, instance) = unsafe {
            let instance = Instance::from_vmctx(vmctx);
            let instance = instance.as_ref();
            let store = &mut *instance.store.unwrap().0.as_ptr();
            (store, instance.id)
        };

        // Thread the `store` and `instance` through panic/trap infrastructure
        // back into `f`.
        catch_unwind_and_record_trap(store, |store| f(store, instance))
    }

    /// Converts the provided `*mut VMContext` to an `Instance` pointer and
    /// returns it with the same lifetime as `self`.
    ///
    /// This function can be used when traversing a `VMContext` to reach into
    /// the context needed for imports, optionally.
    ///
    /// # Safety
    ///
    /// This function requires that the `vmctx` pointer is indeed valid and
    /// from the store that `self` belongs to.
    #[inline]
    unsafe fn sibling_vmctx<'a>(&'a self, vmctx: NonNull<VMContext>) -> &'a Instance {
        // SAFETY: it's a contract of this function itself that `vmctx` is a
        // valid pointer. Additionally with `self` being a
        let ptr = unsafe { Instance::from_vmctx(vmctx) };
        // SAFETY: it's a contract of this function itself that `vmctx` is a
        // valid pointer to dereference. Additionally the lifetime of the return
        // value is constrained to be the same as `self` to avoid granting a
        // too-long lifetime.
        unsafe { ptr.as_ref() }
    }

    /// Same as [`Self::sibling_vmctx`], but the mutable version.
    ///
    /// # Safety
    ///
    /// This function requires that the `vmctx` pointer is indeed valid and
    /// from the store that `self` belongs to.
    ///
    /// (Note that it is *NOT* required that `vmctx` be distinct from this
    /// instance's `vmctx`, or that usage of the resulting instance is limited
    /// to its defined items! The returned borrow has the same lifetime as
    /// `self`, which means that this instance cannot be used while the
    /// resulting instance is in use, and we therefore do not need to worry
    /// about mutable aliasing between this instance and the resulting
    /// instance.)
    #[inline]
    unsafe fn sibling_vmctx_mut<'a>(
        self: Pin<&'a mut Self>,
        vmctx: NonNull<VMContext>,
    ) -> Pin<&'a mut Instance> {
        // SAFETY: it's a contract of this function itself that `vmctx` is a
        // valid pointer such that this pointer arithmetic is valid.
        let mut ptr = unsafe { Instance::from_vmctx(vmctx) };

        // SAFETY: it's a contract of this function itself that `vmctx` is a
        // valid pointer to dereference. Additionally the lifetime of the return
        // value is constrained to be the same as `self` to avoid granting a
        // too-long lifetime. Finally mutable references to an instance are
        // always through `Pin`, so it's safe to create a pin-pointer here.
        unsafe { Pin::new_unchecked(ptr.as_mut()) }
    }

    pub(crate) fn env_module(&self) -> &Arc<wasmtime_environ::Module> {
        self.runtime_info.env_module()
    }

    pub(crate) fn runtime_module(&self) -> Option<&crate::Module> {
        match &self.runtime_info {
            ModuleRuntimeInfo::Module(m) => Some(m),
            ModuleRuntimeInfo::Bare(_) => None,
        }
    }

    /// Translate a module-level interned type index into an engine-level
    /// interned type index.
    #[cfg(feature = "gc")]
    pub fn engine_type_index(&self, module_index: ModuleInternedTypeIndex) -> VMSharedTypeIndex {
        self.runtime_info.engine_type_index(module_index)
    }

    #[inline]
    fn offsets(&self) -> &VMOffsets<HostPtr> {
        self.runtime_info.offsets()
    }

    /// Return the indexed `VMFunctionImport`.
    fn imported_function(&self, index: FuncIndex) -> &VMFunctionImport {
        unsafe { self.vmctx_plus_offset(self.offsets().vmctx_vmfunction_import(index)) }
    }

    /// Return the index `VMTableImport`.
    fn imported_table(&self, index: TableIndex) -> &VMTableImport {
        unsafe { self.vmctx_plus_offset(self.offsets().vmctx_vmtable_import(index)) }
    }

    /// Return the indexed `VMMemoryImport`.
    fn imported_memory(&self, index: MemoryIndex) -> &VMMemoryImport {
        unsafe { self.vmctx_plus_offset(self.offsets().vmctx_vmmemory_import(index)) }
    }

    /// Return the indexed `VMGlobalImport`.
    fn imported_global(&self, index: GlobalIndex) -> &VMGlobalImport {
        unsafe { self.vmctx_plus_offset(self.offsets().vmctx_vmglobal_import(index)) }
    }

    /// Return the indexed `VMTagImport`.
    fn imported_tag(&self, index: TagIndex) -> &VMTagImport {
        unsafe { self.vmctx_plus_offset(self.offsets().vmctx_vmtag_import(index)) }
    }

    /// Return the indexed `VMTagDefinition`.
    pub fn tag_ptr(&self, index: DefinedTagIndex) -> NonNull<VMTagDefinition> {
        unsafe { self.vmctx_plus_offset_raw(self.offsets().vmctx_vmtag_definition(index)) }
    }

    /// Return the indexed `VMTableDefinition`.
    pub fn table(&self, index: DefinedTableIndex) -> VMTableDefinition {
        unsafe { self.table_ptr(index).read() }
    }

    /// Updates the value for a defined table to `VMTableDefinition`.
    fn set_table(self: Pin<&mut Self>, index: DefinedTableIndex, table: VMTableDefinition) {
        unsafe {
            self.table_ptr(index).write(table);
        }
    }

    /// Return a pointer to the `index`'th table within this instance, stored
    /// in vmctx memory.
    pub fn table_ptr(&self, index: DefinedTableIndex) -> NonNull<VMTableDefinition> {
        unsafe { self.vmctx_plus_offset_raw(self.offsets().vmctx_vmtable_definition(index)) }
    }

    /// Get a locally defined or imported memory.
    pub(crate) fn get_memory(&self, index: MemoryIndex) -> VMMemoryDefinition {
        if let Some(defined_index) = self.env_module().defined_memory_index(index) {
            self.memory(defined_index)
        } else {
            let import = self.imported_memory(index);
            unsafe { VMMemoryDefinition::load(import.from.as_ptr()) }
        }
    }

    /// Return the indexed `VMMemoryDefinition`, loaded from vmctx memory
    /// already.
    #[inline]
    pub fn memory(&self, index: DefinedMemoryIndex) -> VMMemoryDefinition {
        unsafe { VMMemoryDefinition::load(self.memory_ptr(index).as_ptr()) }
    }

    /// Set the indexed memory to `VMMemoryDefinition`.
    fn set_memory(&self, index: DefinedMemoryIndex, mem: VMMemoryDefinition) {
        unsafe {
            self.memory_ptr(index).write(mem);
        }
    }

    /// Return the address of the specified memory at `index` within this vmctx.
    ///
    /// Note that the returned pointer resides in wasm-code-readable-memory in
    /// the vmctx.
    #[inline]
    pub fn memory_ptr(&self, index: DefinedMemoryIndex) -> NonNull<VMMemoryDefinition> {
        unsafe {
            self.vmctx_plus_offset::<VmPtr<_>>(self.offsets().vmctx_vmmemory_pointer(index))
                .as_non_null()
        }
    }

    /// Return the indexed `VMGlobalDefinition`.
    pub fn global_ptr(&self, index: DefinedGlobalIndex) -> NonNull<VMGlobalDefinition> {
        unsafe { self.vmctx_plus_offset_raw(self.offsets().vmctx_vmglobal_definition(index)) }
    }

    /// Get all globals within this instance.
    ///
    /// Returns both import and defined globals.
    ///
    /// Returns both exported and non-exported globals.
    ///
    /// Gives access to the full globals space.
    pub fn all_globals(
        &self,
        store: StoreId,
    ) -> impl ExactSizeIterator<Item = (GlobalIndex, crate::Global)> + '_ {
        let module = self.env_module();
        module
            .globals
            .keys()
            .map(move |idx| (idx, self.get_exported_global(store, idx)))
    }

    /// Get the globals defined in this instance (not imported).
    pub fn defined_globals(
        &self,
        store: StoreId,
    ) -> impl ExactSizeIterator<Item = (DefinedGlobalIndex, crate::Global)> + '_ {
        let module = self.env_module();
        self.all_globals(store)
            .skip(module.num_imported_globals)
            .map(move |(i, global)| (module.defined_global_index(i).unwrap(), global))
    }

    /// Return a pointer to the interrupts structure
    #[inline]
    pub fn vm_store_context(&self) -> NonNull<Option<VmPtr<VMStoreContext>>> {
        unsafe { self.vmctx_plus_offset_raw(self.offsets().ptr.vmctx_store_context()) }
    }

    /// Return a pointer to the global epoch counter used by this instance.
    #[cfg(target_has_atomic = "64")]
    pub fn epoch_ptr(self: Pin<&mut Self>) -> &mut Option<VmPtr<AtomicU64>> {
        let offset = self.offsets().ptr.vmctx_epoch_ptr();
        unsafe { self.vmctx_plus_offset_mut(offset) }
    }

    /// Return a pointer to the collector-specific heap data.
    pub fn gc_heap_data(self: Pin<&mut Self>) -> &mut Option<VmPtr<u8>> {
        let offset = self.offsets().ptr.vmctx_gc_heap_data();
        unsafe { self.vmctx_plus_offset_mut(offset) }
    }

    /// Return a mutable reference to the AN-encoding `i32.and` LUT base
    /// pointer slot in this instance's `VMContext`.
    pub fn an_and_table(self: Pin<&mut Self>) -> &mut Option<VmPtr<u32>> {
        let offset = self.offsets().ptr.vmctx_an_and_table();
        unsafe { self.vmctx_plus_offset_mut(offset) }
    }

    /// Return a mutable reference to the AN-encoding `i32.or` LUT base
    /// pointer slot in this instance's `VMContext`.
    pub fn an_or_table(self: Pin<&mut Self>) -> &mut Option<VmPtr<u32>> {
        let offset = self.offsets().ptr.vmctx_an_or_table();
        unsafe { self.vmctx_plus_offset_mut(offset) }
    }

    /// Return a mutable reference to the AN-encoding `i32.xor` LUT base
    /// pointer slot in this instance's `VMContext`.
    pub fn an_xor_table(self: Pin<&mut Self>) -> &mut Option<VmPtr<u32>> {
        let offset = self.offsets().ptr.vmctx_an_xor_table();
        unsafe { self.vmctx_plus_offset_mut(offset) }
    }

    /// Raw `(base, len)` of the AN-encoding shadow buffer for one defined
    /// memory. Returns `None` when no shadow is allocated (AN off or shared;
    /// a defined index of this instance is never imported). The returned pointer is valid until the next
    /// `an_grow_shadow` call on this memory or instance drop. Test-only:
    /// embedders should not depend on the shadow layout.
    pub fn an_shadow_raw_parts_for_test(
        &self,
        memory_index: DefinedMemoryIndex,
    ) -> Option<(*mut u8, usize)> {
        let shadow = self.an_enc_shadows[memory_index].as_deref()?;
        let len = shadow.len();
        Some((shadow.as_ptr() as *mut u8, len))
    }

    pub(crate) unsafe fn set_store(mut self: Pin<&mut Self>, store: &StoreOpaque) {
        // FIXME: should be more targeted ideally with the `unsafe` than just
        // throwing this entire function in a large `unsafe` block.
        unsafe {
            *self.as_mut().store_mut() = Some(VMStoreRawPtr(store.traitobj()));
            self.vm_store_context()
                .write(Some(store.vm_store_context_ptr().into()));
            #[cfg(target_has_atomic = "64")]
            {
                *self.as_mut().epoch_ptr() =
                    Some(NonNull::from(store.engine().epoch_counter()).into());
            }

            if self.env_module().needs_gc_heap {
                self.as_mut().set_gc_heap(Some(store.unwrap_gc_store()));
            } else {
                self.as_mut().set_gc_heap(None);
            }

            self.as_mut().set_an_lut_pointers(store);
            self.as_mut().set_an_enc_shadows(store);
        }
    }

    /// Populate the three AN-encoding LUT base-pointer slots in this
    /// instance's `VMContext` from the engine's per-A tables. When AN-encoding
    /// is off the slots stay `None`.
    fn set_an_lut_pointers(mut self: Pin<&mut Self>, store: &StoreOpaque) {
        use crate::engine::AnLutBinOp;
        let engine = store.engine();
        let to_vmptr = |op| {
            engine
                .an_lut_addr(op)
                .map(|p| VmPtr::from(NonNull::new(p as *mut u32).expect("LUT address non-null")))
        };
        *self.as_mut().an_and_table() = to_vmptr(AnLutBinOp::And);
        *self.as_mut().an_or_table() = to_vmptr(AnLutBinOp::Or);
        *self.as_mut().an_xor_table() = to_vmptr(AnLutBinOp::Xor);
    }

    /// Re-encode a *byte range* of one defined memory's raw buffer into its
    /// AN-encoding shadow. Every 4-byte slot that is partially or fully
    /// contained in `[start, start + len)` is rebuilt as
    /// `A * u32_le(raw[4i..4i+4])` from current raw bytes.
    ///
    /// Used by the bulk-memory libcalls (`memory.copy`, `memory.fill`,
    /// `memory.init`): after the raw-side `ptr::copy` / `ptr::write_bytes`,
    /// this brings every touched shadow slot back into sync so the
    /// invariant `decode(enc_slot) == u32_le(raw_slot)` holds before wasm
    /// resumes.
    ///
    /// No-op if `len == 0`, if the memory has no shadow allocated (AN off or shared;
    /// a defined index of this instance is never imported), or if `start >= raw_len`.
    ///
    /// Returns `false` — *without* re-encoding — when the retained bytes of a
    /// partially covered boundary slot disagree with the shadow (see
    /// [`Self::an_boundary_slots_consistent_parts`]); the divergence evidence
    /// is preserved for the caller to surface as `Trap::AnMemoryMismatch`.
    /// The check is skipped while the memory is whole-dirty (shadow
    /// legitimately stale, raw is the source of truth).
    #[must_use]
    pub(crate) fn an_encode_range_from_raw(
        self: Pin<&mut Self>,
        memory_index: DefinedMemoryIndex,
        start: usize,
        len: usize,
        a: u64,
    ) -> bool {
        let range = start..start.saturating_add(len);
        self.an_encode_ranges_from_raw(memory_index, core::slice::from_ref(&range), a)
    }

    /// Re-encode multiple host-written byte ranges as one operation. Retained
    /// bytes are validated against the union of all ranges before any touched
    /// slot is rebuilt, so separate writes into one 4-byte slot do not treat
    /// each other as corruption while genuinely untouched bytes are still
    /// checked.
    #[must_use]
    pub(crate) fn an_encode_ranges_from_raw(
        mut self: Pin<&mut Self>,
        memory_index: DefinedMemoryIndex,
        ranges: &[Range<usize>],
        a: u64,
    ) -> bool {
        let mem_def = self.memories[memory_index].1.vmmemory();
        let raw_base: *const u8 = mem_def.base.as_ptr();
        let raw_len = mem_def.current_length();
        let whole_dirty = self.an_whole_dirty[memory_index];

        let shadow = match self.as_mut().an_enc_shadows_mut()[memory_index].as_deref_mut() {
            Some(s) => s,
            None => return true,
        };
        if !whole_dirty
            && !Self::an_dirty_ranges_consistent_parts(shadow, raw_base, raw_len, ranges, a)
        {
            return false;
        }
        for range in ranges {
            Self::an_encode_range_parts(
                shadow,
                raw_base,
                raw_len,
                range.start,
                range.end.saturating_sub(range.start),
                a,
            );
        }
        true
    }

    /// Verify the *retained* bytes of the partially covered boundary slots of
    /// `[start, start + len)` against the encoded shadow, before a range
    /// re-encode rebuilds those slots wholly from raw.
    ///
    /// A range re-encode rounds outward to 4-byte slots, so the boundary
    /// slots' bytes *outside* the written range are also re-read from raw.
    /// Without this check, raw corruption in those neighbor bytes would be
    /// re-encoded into a fresh valid codeword — laundered — while the guest's
    /// equivalent path (the JIT sub-word store byte-RMW) validates the old
    /// shadow codeword before merging. Bytes inside `[start, end)` are the
    /// host's new data and are skipped; fully covered slots need no check.
    ///
    /// Returns `true` when consistent or when there is nothing to check.
    fn an_boundary_slots_consistent_parts(
        shadow: &[u8],
        raw_base: *const u8,
        raw_len: usize,
        start: usize,
        len: usize,
        a: u64,
    ) -> bool {
        let range = start..start.saturating_add(len);
        Self::an_dirty_ranges_consistent_parts(
            shadow,
            raw_base,
            raw_len,
            core::slice::from_ref(&range),
            a,
        )
    }

    /// Validate bytes not written by any of `ranges` in every partially dirty
    /// 4-byte slot. All validation happens before the caller re-encodes any
    /// slot, preserving the original shadow if a retained byte disagrees.
    fn an_dirty_ranges_consistent_parts(
        shadow: &[u8],
        raw_base: *const u8,
        raw_len: usize,
        ranges: &[Range<usize>],
        a: u64,
    ) -> bool {
        let mut boundary_slots = Vec::with_capacity(ranges.len().saturating_mul(2));
        for range in ranges {
            let start = range.start;
            let end = range.end.min(raw_len);
            if start >= end {
                continue;
            }
            boundary_slots.push(start / 4);
            boundary_slots.push((end - 1) / 4);
        }
        boundary_slots.sort_unstable();
        boundary_slots.dedup();

        let slot_retained_consistent = |slot: usize| -> bool {
            let raw_off = slot * 4;
            let slot_len = (raw_len - raw_off).min(4);
            let is_dirty = |pos: usize| {
                ranges
                    .iter()
                    .any(|range| range.start <= pos && pos < range.end)
            };
            if (0..slot_len).all(|i| is_dirty(raw_off + i)) {
                return true;
            }
            let enc_off = slot * 8;
            let Some(enc_bytes) = shadow.get(enc_off..enc_off + 8) else {
                return false;
            };
            let enc = u64::from_le_bytes(enc_bytes.try_into().unwrap());
            if enc % a != 0 {
                return false;
            }
            let word = enc / a;
            if word > u64::from(u32::MAX) {
                return false;
            }
            let word_bytes = (word as u32).to_le_bytes();
            for i in 0..slot_len {
                let pos = raw_off + i;
                if is_dirty(pos) {
                    // Being overwritten: raw is the new truth here.
                    continue;
                }
                // SAFETY: `pos < raw_len`; `raw_base` is valid for `raw_len`
                // bytes.
                if unsafe { raw_base.add(pos).read() } != word_bytes[i] {
                    return false;
                }
            }
            true
        };

        boundary_slots.into_iter().all(slot_retained_consistent)
    }

    /// Slot-encode core shared by [`Self::an_encode_range_from_raw`], the
    /// full-memory re-encode, and the imported-memory path (which addresses
    /// the owning instance's shadow through raw parts). Every 4-byte slot
    /// partially or fully contained in `[start, start + len)` (clamped to
    /// `raw_len`) is rebuilt as `A * u32_le(raw[4i..4i+4])`.
    fn an_encode_range_parts(
        shadow: &mut [u8],
        raw_base: *const u8,
        raw_len: usize,
        start: usize,
        len: usize,
        a: u64,
    ) {
        if len == 0 || start >= raw_len {
            return;
        }
        let end = start.saturating_add(len).min(raw_len);

        let first_slot = start / 4;
        let last_slot_exclusive = (end + 3) / 4;

        for slot in first_slot..last_slot_exclusive {
            let raw_off = slot * 4;
            let raw_u32 = if raw_len - raw_off >= 4 {
                // SAFETY: `raw_off + 4 <= raw_len`. `raw_base` is valid for
                // `raw_len` bytes.
                u32::from_le(unsafe { raw_base.add(raw_off).cast::<u32>().read_unaligned() })
            } else {
                // Tail slot shorter than 4 raw bytes: zero-pad. Only
                // reachable when `raw_len` is not a multiple of 4, i.e. with
                // the custom-page-sizes proposal (any index type). NB: the
                // shadow is sized `2 * raw_len`, which is too small for the
                // rounded-up slot count in that case — see the
                // custom-page-sizes finding; until that is fixed (or the
                // proposal refused under AN) this arm can index past the
                // shadow and panic.
                let mut bytes = [0u8; 4];
                let to_copy = raw_len - raw_off;
                // SAFETY: `raw_off + to_copy <= raw_len`. `raw_base` is valid
                // for `raw_len` bytes.
                unsafe {
                    ptr::copy_nonoverlapping(raw_base.add(raw_off), bytes.as_mut_ptr(), to_copy);
                }
                u32::from_le_bytes(bytes)
            };
            let encoded = a.wrapping_mul(u64::from(raw_u32));
            let enc_off = slot * 8;
            shadow[enc_off..enc_off + 8].copy_from_slice(&encoded.to_le_bytes());
        }
    }

    /// Re-allocate the AN-encoding shadow for a defined memory whose raw
    /// buffer has grown via `memory.grow`. Allocates a fresh
    /// `2 * new_raw_size` `Box<[u8]>`, copies the previous shadow's
    /// already-encoded bytes into it, swaps it in, and updates the
    /// `vmctx_an_enc_memory_base` slot.
    ///
    /// No-op when the memory has no shadow allocated (AN off or shared;
    /// a defined index of this instance is never imported).
    pub(crate) fn an_grow_shadow(mut self: Pin<&mut Self>, memory_index: DefinedMemoryIndex) {
        if self.an_enc_shadows[memory_index].is_none() {
            return;
        }
        let raw_len = self.memories[memory_index].1.byte_size();
        let new_size = raw_len
            .checked_mul(usize::try_from(wasmtime_environ::ENC_MEM_GROWTH_FACTOR).unwrap())
            .expect("AN shadow size overflowed usize on memory.grow");
        let mut new_shadow: Box<[u8]> = vec![0u8; new_size].into_boxed_slice();

        // `memory.grow` only appends fresh zero pages; every pre-existing raw
        // byte keeps its offset, so the old shadow is still a valid encoding of
        // those bytes. Copy them verbatim into the new buffer and leave the
        // rest zero: the grown raw pages are zero and `A * 0 == 0`, so their
        // shadow slots are correctly zero straight from the fresh allocation.
        //
        // This deliberately avoids re-encoding the whole memory from raw on
        // every grow. That re-encode was O(raw_len): for a multi-GiB memory it
        // read every raw byte and wrote every shadow byte, committing both
        // buffers and turning an otherwise lazy / VM-based `memory.grow` into a
        // multi-second, multi-GiB spin (e.g. the `big-strings` component test).
        // Copying only touches the old shadow (the bytes already resident),
        // leaving the freshly mapped tail untouched and lazily zero-backed.
        //
        // It also keeps the encoded shadow as the source of truth: re-encoding
        // from raw would silently "heal" any raw corruption that verify-at-use
        // (the guest load check / host read verify) is supposed to detect.
        if let Some(old_shadow) = self.an_enc_shadows[memory_index].as_deref() {
            let n = old_shadow.len().min(new_shadow.len());
            new_shadow[..n].copy_from_slice(&old_shadow[..n]);
        }

        let base_ptr = NonNull::new(new_shadow.as_ptr() as *mut u8)
            .expect("AN shadow grow allocation produced a null pointer");
        self.as_mut().an_enc_shadows_mut()[memory_index] = Some(new_shadow);

        // Update the `VMContext` slot so JIT-emitted stores see the new
        // base pointer.
        let offsets = *self.runtime_info.offsets();
        // SAFETY: slot is sized + addressable via `vmctx_an_enc_memory_base`,
        // and `NonNull::write` initializes without reading the prior value
        // (defensive — the slot already holds a valid value here).
        unsafe {
            let slot: NonNull<Option<VmPtr<u8>>> = self
                .as_mut()
                .vmctx_plus_offset_raw(offsets.vmctx_an_enc_memory_base(memory_index));
            slot.write(Some(VmPtr::from(base_ptr)));
        }
    }

    /// Whether an AN-encoding shadow is allocated for this defined memory
    /// (true only when AN-encoding is on and the memory is not shared).
    pub(crate) fn an_has_shadow(&self, memory_index: DefinedMemoryIndex) -> bool {
        self.an_enc_shadows[memory_index].is_some()
    }

    /// The AN-encoding shadow buffer of one defined memory as an immutable
    /// slice, or `None` when no shadow is allocated (AN off / shared). Used by the component lifting read-verify to cross-check a
    /// lifted range against the encoded shadow before the bytes reach the host.
    pub(crate) fn an_shadow_slice(&self, memory_index: DefinedMemoryIndex) -> Option<&[u8]> {
        self.an_enc_shadows[memory_index].as_deref()
    }

    /// Raw `(shadow_base, raw_base, raw_len)` of an *imported* memory's
    /// AN-encoding shadow, read through the
    /// `VMMemoryImport::an_enc_base_slot` indirection into the owning
    /// instance. Returns `None` when the import carries no slot pointer
    /// (AN-encoding off, or shared memory).
    pub(crate) fn an_imported_memory_shadow_parts(
        &self,
        index: MemoryIndex,
    ) -> Option<(*mut u8, *const u8, usize)> {
        let import = self.imported_memory(index);
        let slot_ptr = import.an_enc_base_slot?;
        // SAFETY: `slot_ptr` targets the owning instance's enc-base
        // `VMContext` slot (an `Option<VmPtr<u8>>`), valid for the owner's
        // lifetime, which outlives this import. The slot is initialized in
        // the owner's `set_an_enc_shadows` before any import is wired up.
        let shadow_base = unsafe { slot_ptr.as_ptr().cast::<Option<VmPtr<u8>>>().read() }?;
        // SAFETY: `from` points at the owner's live `VMMemoryDefinition`.
        let def = unsafe { VMMemoryDefinition::load(import.from.as_ptr()) };
        Some((
            shadow_base.as_ptr(),
            def.base.as_ptr() as *const u8,
            def.current_length(),
        ))
    }

    /// Re-encode `[start, start + len)` of an imported memory's raw bytes
    /// into the owning instance's shadow. Used by the bulk-memory libcalls
    /// whose destination resolves to an imported memory. No-op when no
    /// shadow exists.
    ///
    /// Like [`Self::an_encode_range_from_raw`], returns `false` without
    /// re-encoding when a partially covered boundary slot's retained bytes
    /// disagree with the shadow. No whole-dirty consultation is needed: this
    /// runs only from bulk-memory libcalls (i.e. during wasm execution), and
    /// whole-dirty is always healed before wasm re-enters.
    #[must_use]
    pub(crate) fn an_encode_imported_range_from_raw(
        &self,
        index: MemoryIndex,
        start: usize,
        len: usize,
        a: u64,
    ) -> bool {
        let Some((shadow_base, raw_base, raw_len)) = self.an_imported_memory_shadow_parts(index)
        else {
            return true;
        };
        // SAFETY: the owner's shadow is `2 * raw_len` bytes by the layout
        // invariant and alive for the owner's lifetime; exclusive store
        // access makes the temporary `&mut` to the owner's shadow unique.
        let shadow = unsafe { core::slice::from_raw_parts_mut(shadow_base, raw_len * 2) };
        if !Self::an_boundary_slots_consistent_parts(shadow, raw_base, raw_len, start, len, a) {
            return false;
        }
        Self::an_encode_range_parts(shadow, raw_base, raw_len, start, len, a);
        true
    }

    /// Cross-check `[start, start + len)` of an *imported* memory's raw bytes
    /// against the owning instance's shadow — the read-twin of
    /// [`Self::an_encode_imported_range_from_raw`]. Returns `true` when every
    /// touched slot agrees, or when no shadow is allocated (AN off / shared).
    /// Used by the `memory.copy` source verify-at-use when the source resolves
    /// to an imported memory.
    pub(crate) fn an_cross_check_imported_range(
        &self,
        index: MemoryIndex,
        start: usize,
        len: usize,
        a: u64,
    ) -> bool {
        let Some((shadow_base, raw_base, raw_len)) = self.an_imported_memory_shadow_parts(index)
        else {
            return true;
        };
        // SAFETY: the owner's shadow is `2 * raw_len` bytes by the layout
        // invariant and alive for the owner's lifetime; we only read it.
        let shadow = unsafe { core::slice::from_raw_parts(shadow_base as *const u8, raw_len * 2) };
        Self::an_cross_check_range_parts(shadow, raw_base, raw_len, start, len, a)
    }

    /// Mark one defined memory's AN-encoding shadow as wholly out-of-sync
    /// with raw memory.
    ///
    /// Called when the host obtains an untracked whole-memory mutable view
    /// (`Memory::data_mut` / `Memory::data_and_store_mut`); the host may
    /// write anywhere through that borrow, so the next
    /// `an_resync_host_boundary` must re-encode the whole memory from raw.
    /// No-op when the memory has no shadow allocated (AN off or shared;
    /// a defined index of this instance is never imported).
    pub(crate) fn an_mark_whole_dirty(self: Pin<&mut Self>, memory_index: DefinedMemoryIndex) {
        if self.an_enc_shadows[memory_index].is_none() {
            return;
        }
        // SAFETY: `an_whole_dirty` is a plain field with no pinning
        // requirements (see `store_mut`).
        unsafe {
            self.get_unchecked_mut().an_whole_dirty[memory_index] = true;
        }
    }

    /// Read and clear the whole-dirty flag for one defined memory. Used by
    /// the `an_resync_host_boundary` libcall to decide whether a full
    /// re-encode from raw is required.
    pub(crate) fn an_take_whole_dirty(
        self: Pin<&mut Self>,
        memory_index: DefinedMemoryIndex,
    ) -> bool {
        // SAFETY: `an_whole_dirty` is a plain field with no pinning
        // requirements (see `store_mut`).
        let flag = unsafe { &mut self.get_unchecked_mut().an_whole_dirty[memory_index] };
        core::mem::replace(flag, false)
    }

    /// Peek (without clearing) whether one defined memory is whole-dirty.
    ///
    /// A whole-dirty memory has had its raw bytes written through an untracked
    /// host borrow (`Memory::data_mut`) and is pending a full re-encode, so its
    /// shadow is *legitimately* stale. Host read-side verify-at-use checks
    /// consult this to skip a memory whose shadow is known-stale (raw is the
    /// source of truth there) rather than mistaking the staleness for
    /// corruption. Returns `false` when the memory has no shadow.
    pub(crate) fn an_is_whole_dirty(&self, memory_index: DefinedMemoryIndex) -> bool {
        self.an_whole_dirty[memory_index]
    }

    /// AN-encoding cross-check for a whole defined memory.
    ///
    /// Asserts every 8-byte shadow slot equals `A * u32_le(raw[4i..4i+4])`.
    /// Returns `false` on the first mismatch, `true` if all agree or the
    /// memory has no shadow (AN off / shared). Used by the opaque
    /// whole-memory host borrows (`Memory::data` / `data_mut`), whose read
    /// range cannot be known. Thin wrapper over [`Self::an_cross_check_range`]
    /// (whole memory = the full range) so there is a single slot-compare
    /// implementation.
    pub(crate) fn an_cross_check_memory(&self, memory_index: DefinedMemoryIndex, a: u64) -> bool {
        // `a == 0` is rejected by the config setter; the surrounding plumbing
        // guarantees a non-zero `a` here.
        debug_assert!(a != 0, "AN constant A=0 reached an_cross_check_memory");
        // The setter also enforces `a < 2^24`; the mul+compare relies on it
        // (`a * raw < 2^56` cannot wrap a u64).
        debug_assert!(
            a < (1 << 24),
            "AN constant A >= 2^24 reached an_cross_check_memory"
        );
        self.an_cross_check_range(memory_index, 0, usize::MAX, a)
    }

    /// AN-encoding cross-check restricted to the 4-byte slots overlapping
    /// `[start, start + len)` (clamped to the memory's current length).
    ///
    /// Asserts `enc_slot == A * u32_le(raw_slot)` for exactly the slots a host
    /// is about to read (verify-at-use), instead of sweeping the whole memory.
    /// `enc_slot == A * raw` is exactly equivalent to
    /// `enc_slot % A == 0 && enc_slot / A == raw` because `A < 2^24` keeps the
    /// product below `2^56` (no u64 wrap). Returns `true` when consistent, when
    /// the memory has no shadow (AN off / shared), or when the range
    /// is empty / entirely past the end.
    ///
    /// Mirrors [`Self::an_encode_range_parts`] for slot selection and tail
    /// (zero-pad) handling, so the verified slots are exactly those the
    /// re-encode would have touched.
    pub(crate) fn an_cross_check_range(
        &self,
        memory_index: DefinedMemoryIndex,
        start: usize,
        len: usize,
        a: u64,
    ) -> bool {
        let mem_def = self.memories[memory_index].1.vmmemory();
        let raw_base: *const u8 = mem_def.base.as_ptr();
        let raw_len = mem_def.current_length();

        let shadow = match self.an_enc_shadows[memory_index].as_deref() {
            Some(s) => s,
            None => return true,
        };
        Self::an_cross_check_range_parts(shadow, raw_base, raw_len, start, len, a)
    }

    /// Range cross-check core shared by [`Self::an_cross_check_range`]. Every
    /// 4-byte slot partially or fully contained in `[start, start + len)`
    /// (clamped to `raw_len`) is compared against its 8-byte shadow slot.
    ///
    /// `pub(crate)` so the component lifting read-verify can reuse the single
    /// slot-compare implementation against raw + shadow slices it already
    /// holds (rather than re-deriving the memory index).
    pub(crate) fn an_cross_check_range_parts(
        shadow: &[u8],
        raw_base: *const u8,
        raw_len: usize,
        start: usize,
        len: usize,
        a: u64,
    ) -> bool {
        if len == 0 || start >= raw_len {
            return true;
        }
        let end = start.saturating_add(len).min(raw_len);
        let first_slot = start / 4;
        let last_slot_exclusive = (end + 3) / 4;

        for slot in first_slot..last_slot_exclusive {
            let raw_off = slot * 4;
            let raw_u32 = if raw_len - raw_off >= 4 {
                // SAFETY: `raw_off + 4 <= raw_len`; `raw_base` is valid for
                // `raw_len` bytes for the duration of this call.
                u32::from_le(unsafe { raw_base.add(raw_off).cast::<u32>().read_unaligned() })
            } else {
                let mut bytes = [0u8; 4];
                let to_copy = raw_len - raw_off;
                // SAFETY: `raw_off + to_copy <= raw_len`.
                unsafe {
                    ptr::copy_nonoverlapping(raw_base.add(raw_off), bytes.as_mut_ptr(), to_copy);
                }
                u32::from_le_bytes(bytes)
            };
            let enc_off = slot * 8;
            let enc_slot = u64::from_le_bytes(
                shadow[enc_off..enc_off + 8]
                    .try_into()
                    .expect("8 bytes from shadow"),
            );
            if enc_slot != a.wrapping_mul(u64::from(raw_u32)) {
                return false;
            }
        }
        true
    }

    /// Re-encode every 4-byte slot of one defined memory's raw buffer into
    /// its AN-encoding shadow. For each slot `i`, writes
    /// `A * u32_le(raw[4i..4i+4])` as a little-endian `u64` into
    /// `shadow[8i..8i+8]`.
    ///
    /// Tail bytes that don't fill a 4-byte raw slot are padded with zeros
    /// before encoding (this matters for memory64 modules using a custom
    /// page size; default 64 KiB pages are always a multiple of 4).
    ///
    /// No-op when AN-encoding is off (the shadow slot is `None`). Idempotent.
    ///
    /// Used at the end of initial memory initialization and at host-call exit
    /// to make the shadow reflect the current raw bytes after a host might
    /// have written through `Memory::data_mut`.
    pub(crate) fn an_encode_full_memory_from_raw(
        mut self: Pin<&mut Self>,
        memory_index: DefinedMemoryIndex,
        a: u64,
    ) {
        // Snapshot the raw base + length. We hold a pin-projected `&mut self`
        // but only need the raw bytes immutably for the read side.
        let mem_def = self.memories[memory_index].1.vmmemory();
        let raw_base: *const u8 = mem_def.base.as_ptr();
        let raw_len = mem_def.current_length();

        let shadow = match self.as_mut().an_enc_shadows_mut()[memory_index].as_deref_mut() {
            Some(s) => s,
            None => return,
        };
        let expected_shadow_len = raw_len
            .checked_mul(usize::try_from(wasmtime_environ::ENC_MEM_GROWTH_FACTOR).unwrap())
            .expect("shadow size overflow");
        debug_assert_eq!(
            shadow.len(),
            expected_shadow_len,
            "AN-encoding shadow has wrong size for memory {:?}: raw={} shadow={}",
            memory_index,
            raw_len,
            shadow.len()
        );

        Self::an_encode_range_parts(shadow, raw_base, raw_len, 0, raw_len, a);
    }

    /// Allocate the AN-encoding shadow buffers for each defined memory and
    /// write the base pointers into the matching `VMContext` slots.
    ///
    /// Each shadow is sized at `ENC_MEM_GROWTH_FACTOR * raw_byte_size` and
    /// owns its storage as a `Box<[u8]>` held in `self.an_enc_shadows`. The
    /// raw pointer to that storage is mirrored into
    /// `vmctx_an_enc_memory_base(idx)` so JIT-emitted load/store paths can
    /// read it via a single `load.i64` from the vmctx.
    ///
    /// Skips allocation for shared memories (they cannot be cross-checked
    /// against a private shadow without a per-slot lock; refused at compile
    /// time) and is a no-op when AN-encoding is disabled.
    fn set_an_enc_shadows(mut self: Pin<&mut Self>, store: &StoreOpaque) {
        if !store.engine().tunables().an_encoding {
            return;
        }
        let module = self.runtime_info.env_module().clone();
        let offsets = *self.runtime_info.offsets();
        let num_defined_memories = module.num_defined_memories();
        for i in 0..num_defined_memories {
            let defined_memory_index = DefinedMemoryIndex::new(i);
            let memory_index = module.memory_index(defined_memory_index);
            if module.memories[memory_index].shared {
                // Shared memories are refused at compile time when AN-encoding
                // is enabled. Defensive bail-out so this code stays correct if
                // the refusal is ever relaxed without first adding atomic-safe
                // shadow support.
                continue;
            }
            let raw_byte_size = self.memories[defined_memory_index].1.byte_size();
            let shadow_bytes = raw_byte_size
                .checked_mul(usize::try_from(wasmtime_environ::ENC_MEM_GROWTH_FACTOR).unwrap())
                .expect("shadow size overflowed usize");
            // Allocate zero-filled. Zero decodes to zero under the AN encoding
            // (`A * 0 == 0`), so a freshly created memory's shadow is already
            // consistent with the raw side without any extra work.
            let shadow: Box<[u8]> = vec![0u8; shadow_bytes].into_boxed_slice();
            let base_ptr = NonNull::new(shadow.as_ptr() as *mut u8)
                .expect("shadow allocation produced a null pointer");
            self.as_mut().an_enc_shadows_mut()[defined_memory_index] = Some(shadow);
            // SAFETY: the slot lives in this instance's `VMContext`, sized by
            // `VMOffsets::defined_memories_enc_bases`. We must use a raw
            // `NonNull::write` here (not `&mut *slot = ...`) because
            // `set_an_enc_shadows` runs from inside `set_store`, which is
            // itself called by `initialize_vmctx` *before* the explicit
            // `defined_memories_enc_bases` zero-fill loop further down in
            // `initialize_vmctx`. Forming a `&mut Option<VmPtr<u8>>` to
            // uninitialized memory would be UB; `NonNull::write` instead
            // initializes the slot without reading the prior value. The
            // pointer remains valid for as long as `self.an_enc_shadows`
            // owns the `Box`, which is for the rest of the instance's
            // lifetime.
            unsafe {
                let slot: NonNull<Option<VmPtr<u8>>> = self
                    .as_mut()
                    .vmctx_plus_offset_raw(offsets.vmctx_an_enc_memory_base(defined_memory_index));
                slot.write(Some(VmPtr::from(base_ptr)));
            }
        }
    }

    unsafe fn set_gc_heap(self: Pin<&mut Self>, gc_store: Option<&GcStore>) {
        if let Some(gc_store) = gc_store {
            *self.gc_heap_data() = Some(unsafe { gc_store.gc_heap.vmctx_gc_heap_data().into() });
        } else {
            *self.gc_heap_data() = None;
        }
    }

    /// Return a reference to the vmctx used by compiled wasm code.
    #[inline]
    pub fn vmctx(&self) -> NonNull<VMContext> {
        InstanceLayout::vmctx(self)
    }

    /// Lookup a function by index.
    ///
    /// # Panics
    ///
    /// Panics if `index` is out of bounds for this instance.
    ///
    /// # Safety
    ///
    /// The `store` parameter must be the store that owns this instance and the
    /// functions that this instance can reference.
    pub unsafe fn get_exported_func(
        self: Pin<&mut Self>,
        registry: &ModuleRegistry,
        store: StoreId,
        index: FuncIndex,
    ) -> crate::Func {
        let func_ref = self.get_func_ref(registry, index).unwrap();

        // SAFETY: the validity of `func_ref` is guaranteed by the validity of
        // `self`, and the contract that `store` must own `func_ref` is a
        // contract of this function itself.
        unsafe { crate::Func::from_vm_func_ref(store, func_ref) }
    }

    /// Lookup a table by index.
    ///
    /// # Panics
    ///
    /// Panics if `index` is out of bounds for this instance.
    pub fn get_exported_table(&self, store: StoreId, index: TableIndex) -> crate::Table {
        let (id, def_index) = if let Some(def_index) = self.env_module().defined_table_index(index)
        {
            (self.id, def_index)
        } else {
            let import = self.imported_table(index);
            // SAFETY: validity of this `Instance` guarantees validity of the
            // `vmctx` pointer being read here to find the transitive
            // `InstanceId` that the import is associated with.
            let id = unsafe { self.sibling_vmctx(import.vmctx.as_non_null()).id };
            (id, import.index)
        };
        crate::Table::from_raw(StoreInstanceId::new(store, id), def_index)
    }

    /// Lookup a memory by index.
    ///
    /// # Panics
    ///
    /// Panics if `index` is out-of-bounds for this instance.
    #[cfg_attr(
        not(feature = "threads"),
        expect(unused_variables, reason = "definitions cfg'd to dummy",)
    )]
    pub fn get_exported_memory(&self, store: StoreId, index: MemoryIndex) -> ExportMemory {
        let module = self.env_module();
        if module.memories[index].shared {
            let (memory, import) =
                if let Some(def_index) = self.env_module().defined_memory_index(index) {
                    (
                        self.get_defined_memory(def_index),
                        self.get_defined_memory_vmimport(def_index),
                    )
                } else {
                    let import = self.imported_memory(index);
                    // SAFETY: validity of this `Instance` guarantees validity of
                    // the `vmctx` pointer being read here to find the transitive
                    // `InstanceId` that the import is associated with.
                    let instance = unsafe { self.sibling_vmctx(import.vmctx.as_non_null()) };
                    (instance.get_defined_memory(import.index), *import)
                };

            let vm = memory.as_shared_memory().unwrap().clone();
            ExportMemory::Shared(vm, import)
        } else {
            let (id, def_index) =
                if let Some(def_index) = self.env_module().defined_memory_index(index) {
                    (self.id, def_index)
                } else {
                    let import = self.imported_memory(index);
                    // SAFETY: validity of this `Instance` guarantees validity of the
                    // `vmctx` pointer being read here to find the transitive
                    // `InstanceId` that the import is associated with.
                    let id = unsafe { self.sibling_vmctx(import.vmctx.as_non_null()).id };
                    (id, import.index)
                };

            // SAFETY: `from_raw` requires that the memory is not shared, which
            // was tested above in this if/else.
            let store_id = StoreInstanceId::new(store, id);
            ExportMemory::Unshared(unsafe { crate::Memory::from_raw(store_id, def_index) })
        }
    }

    /// Lookup a global by index.
    ///
    /// # Panics
    ///
    /// Panics if `index` is out-of-bounds for this instance.
    pub(crate) fn get_exported_global(&self, store: StoreId, index: GlobalIndex) -> crate::Global {
        // If this global is defined within this instance, then that's easy to
        // calculate the `Global`.
        if let Some(def_index) = self.env_module().defined_global_index(index) {
            let instance = StoreInstanceId::new(store, self.id);
            return crate::Global::from_core(instance, def_index);
        }

        // For imported globals it's required to match on the `kind` to
        // determine which `Global` constructor is going to be invoked.
        let import = self.imported_global(index);
        match import.kind {
            VMGlobalKind::Host(index) => crate::Global::from_host(store, index),
            VMGlobalKind::Instance(index) => {
                // SAFETY: validity of this `&Instance` means validity of its
                // imports meaning we can read the id of the vmctx within.
                let id = unsafe {
                    let vmctx = VMContext::from_opaque(import.vmctx.unwrap().as_non_null());
                    self.sibling_vmctx(vmctx).id
                };
                crate::Global::from_core(StoreInstanceId::new(store, id), index)
            }
            #[cfg(feature = "component-model")]
            VMGlobalKind::ComponentFlags(index) => {
                // SAFETY: validity of this `&Instance` means validity of its
                // imports meaning we can read the id of the vmctx within.
                let id = unsafe {
                    let vmctx = super::component::VMComponentContext::from_opaque(
                        import.vmctx.unwrap().as_non_null(),
                    );
                    super::component::ComponentInstance::vmctx_instance_id(vmctx)
                };
                crate::Global::from_component_flags(
                    crate::component::store::StoreComponentInstanceId::new(store, id),
                    index,
                )
            }
            #[cfg(feature = "component-model")]
            VMGlobalKind::TaskMayBlock => {
                // SAFETY: validity of this `&Instance` means validity of its
                // imports meaning we can read the id of the vmctx within.
                let id = unsafe {
                    let vmctx = super::component::VMComponentContext::from_opaque(
                        import.vmctx.unwrap().as_non_null(),
                    );
                    super::component::ComponentInstance::vmctx_instance_id(vmctx)
                };
                crate::Global::from_task_may_block(
                    crate::component::store::StoreComponentInstanceId::new(store, id),
                )
            }
        }
    }

    /// Get an exported tag by index.
    ///
    /// # Panics
    ///
    /// Panics if the index is out-of-range.
    pub fn get_exported_tag(&self, store: StoreId, index: TagIndex) -> crate::Tag {
        let (id, def_index) = if let Some(def_index) = self.env_module().defined_tag_index(index) {
            (self.id, def_index)
        } else {
            let import = self.imported_tag(index);
            // SAFETY: validity of this `Instance` guarantees validity of the
            // `vmctx` pointer being read here to find the transitive
            // `InstanceId` that the import is associated with.
            let id = unsafe { self.sibling_vmctx(import.vmctx.as_non_null()).id };
            (id, import.index)
        };
        crate::Tag::from_raw(StoreInstanceId::new(store, id), def_index)
    }

    /// Grow memory by the specified amount of pages.
    ///
    /// Returns `None` if memory can't be grown by the specified amount
    /// of pages. Returns `Some` with the old size in bytes if growth was
    /// successful.
    pub(crate) async fn memory_grow(
        mut self: Pin<&mut Self>,
        limiter: Option<&mut StoreResourceLimiter<'_>>,
        idx: DefinedMemoryIndex,
        delta: u64,
    ) -> Result<Option<usize>, Error> {
        let memory = &mut self.as_mut().memories_mut()[idx].1;

        // SAFETY: this is the safe wrapper around `Memory::grow` because it
        // automatically updates the `VMMemoryDefinition` in this instance after
        // a growth operation below.
        let result = unsafe { memory.grow(delta, limiter).await };

        // Update the state used by a non-shared Wasm memory in case the base
        // pointer and/or the length changed.
        if memory.as_shared_memory().is_none() {
            let vmmemory = memory.vmmemory();
            self.as_mut().set_memory(idx, vmmemory);
        }

        // AN-encoding: re-allocate the shadow to match the new raw size and
        // update the `VMContext` base-pointer slot. Hooked here (not in the
        // `memory_grow` libcall) so every grow path is covered: the wasm
        // `memory.grow` libcall, the embedder-facing `Memory::grow`, and
        // grows on imported memories routed to the owning instance. No-op
        // when the memory has no shadow (AN-encoding off, or shared). Only
        // runs on success — on failure the raw allocation is unchanged so
        // the shadow is still consistent.
        if matches!(result, Ok(Some(_))) {
            self.an_grow_shadow(idx);
        }

        result
    }

    pub(crate) fn table_element_type(
        self: Pin<&mut Self>,
        table_index: TableIndex,
    ) -> TableElementType {
        self.get_table(table_index).element_type()
    }

    /// Performs a grow operation on the `table_index` specified using `grow`.
    ///
    /// This will handle updating the VMTableDefinition internally as necessary.
    pub(crate) async fn defined_table_grow(
        mut self: Pin<&mut Self>,
        table_index: DefinedTableIndex,
        grow: impl AsyncFnOnce(&mut Table) -> Result<Option<usize>>,
    ) -> Result<Option<usize>> {
        let table = self.as_mut().get_defined_table(table_index);
        let result = grow(table).await;
        let element = table.vmtable();
        self.set_table(table_index, element);
        result
    }

    fn alloc_layout(offsets: &VMOffsets<HostPtr>) -> Layout {
        let size = mem::size_of::<Self>()
            .checked_add(usize::try_from(offsets.size_of_vmctx()).unwrap())
            .unwrap();
        let align = mem::align_of::<Self>();
        Layout::from_size_align(size, align).unwrap()
    }

    fn type_ids_array(&self) -> NonNull<VmPtr<VMSharedTypeIndex>> {
        unsafe { self.vmctx_plus_offset_raw(self.offsets().ptr.vmctx_type_ids_array()) }
    }

    /// Get a `&VMFuncRef` for the given `FuncIndex`.
    ///
    /// Returns `None` if the index is the reserved index value.
    ///
    /// The returned reference is a stable reference that won't be moved and can
    /// be passed into JIT code.
    pub(crate) fn get_func_ref(
        self: Pin<&mut Self>,
        registry: &ModuleRegistry,
        index: FuncIndex,
    ) -> Option<NonNull<VMFuncRef>> {
        if index == FuncIndex::reserved_value() {
            return None;
        }

        let Some(def_index) = self.env_module().defined_func_index(index) else {
            debug_assert!(self.env_module().is_imported_function(index));
            return Some(self.imported_function(index).as_func_ref().into());
        };

        // For now, we eagerly initialize an funcref struct in-place whenever
        // asked for a reference to it. This is mostly fine, because in practice
        // each funcref is unlikely to be requested more than a few times:
        // once-ish for funcref tables used for call_indirect (the usual
        // compilation strategy places each function in the table at most once),
        // and once or a few times when fetching exports via API.  Note that for
        // any case driven by table accesses, the lazy table init behaves like a
        // higher-level cache layer that protects this initialization from
        // happening multiple times, via that particular table at least.
        //
        // When `ref.func` becomes more commonly used or if we otherwise see a
        // use-case where this becomes a hotpath, we can reconsider by using
        // some state to track "uninitialized" explicitly, for example by
        // zeroing the funcrefs (perhaps together with other
        // zeroed-at-instantiate-time state) or using a separate is-initialized
        // bitmap.
        //
        // We arrived at this design because zeroing memory is expensive, so
        // it's better for instantiation performance if we don't have to track
        // "is-initialized" state at all!

        let func = &self.env_module().functions[index];
        let type_index = func.signature.unwrap_engine_type_index();

        let module_with_code = ModuleWithCode::in_store(
            registry,
            self.runtime_module()
                .expect("funcref impossible in fake module"),
        )
        .expect("module not in store");

        let array_call = VmPtr::from(
            NonNull::from(
                module_with_code
                    .array_to_wasm_trampoline(def_index)
                    .expect("should have array-to-Wasm trampoline for escaping function"),
            )
            .cast(),
        );

        let wasm_call = Some(VmPtr::from(
            NonNull::new(
                module_with_code
                    .finished_function(def_index)
                    .as_ptr()
                    .cast::<VMWasmCallFunction>()
                    .cast_mut(),
            )
            .unwrap(),
        ));

        let vmctx = VMOpaqueContext::from_vmcontext(self.vmctx()).into();

        // SAFETY: the offset calculated here should be correct with
        // `self.offsets`
        let func_ref_ptr = unsafe {
            self.vmctx_plus_offset_raw::<VMFuncRef>(self.offsets().vmctx_func_ref(func.func_ref))
        };

        // SAFETY: the `func_ref_ptr` should be valid as it's within our
        // `VMContext` area.
        unsafe {
            func_ref_ptr.write(VMFuncRef {
                array_call,
                wasm_call,
                vmctx,
                type_index,
            });
        }

        Some(func_ref_ptr)
    }

    /// Get the passive elements segment at the given index.
    pub(crate) fn passive_element_segment(&self, elem_index: ElemIndex) -> &[ValRaw] {
        let Some(passive) = self
            .env_module()
            .passive_elements_map
            .get(&elem_index)
            .copied()
        else {
            return &[];
        };

        self.passive_elements[passive.index()].elements()
    }

    pub(crate) fn passive_elements_mut(
        self: Pin<&mut Self>,
    ) -> Pin<&mut TryVec<PassiveElementSegment>> {
        // SAFETY: Not moving data out of `self`.
        Pin::new(&mut unsafe { self.get_unchecked_mut() }.passive_elements)
    }

    /// The `table.init` operation: initializes a portion of a table with a
    /// passive element.
    ///
    /// # Errors
    ///
    /// Returns a `Trap` error when the range within the table is out of bounds
    /// or the range within the passive element is out of bounds.
    pub(crate) fn table_init(
        store: &mut StoreOpaque,
        instance_id: InstanceId,
        table_index: TableIndex,
        elem_index: ElemIndex,
        dst: u64,
        src: u64,
        len: u64,
    ) -> Result<()> {
        let mut store = OpaqueRootScope::new(store);
        let store_id = store.id();
        let instance = store.instance(instance_id);
        let elements = instance.passive_element_segment(elem_index);

        let end = dst.checked_add(len).ok_or_else(|| Trap::TableOutOfBounds)?;
        let src = usize::try_from(src).map_err(|_| Trap::TableOutOfBounds)?;
        let len = usize::try_from(len).map_err(|_| Trap::TableOutOfBounds)?;

        let table = instance.get_exported_table(store_id, table_index);
        if end > table.size_(&store) {
            return Err(Trap::TableOutOfBounds.into());
        }

        // Subslice into just the target elements.
        let elements = elements
            .get(src..)
            .and_then(|elements| elements.get(..len))
            .ok_or_else(|| Trap::TableOutOfBounds)?
            .iter()
            .copied()
            .try_collect::<TryVec<_>, OutOfMemory>()?;

        let elem_ty = ValType::from(table.ty_(&store).element().clone());

        let refs = {
            let mut store = AutoAssertNoGc::new(&mut store);
            elements
                .into_iter()
                // SAFETY: the raw elements are valid because we got them from
                // this instance.
                .map(|raw| unsafe { Val::_from_raw(&mut store, raw, &elem_ty) })
                .map(|v| v.ref_().expect("due to validation"))
                .try_collect::<TryVec<_>, OutOfMemory>()?
        };

        let instance = store.instance(instance_id);
        let table = instance.get_exported_table(store_id, table_index);

        for (i, r) in refs.into_iter().enumerate() {
            let i = u64::try_from(i)
                .expect("okay because of `src` and `len` conversions to `usize` up above");
            let j = i
                .checked_add(dst)
                .expect("okay because of `checked_add` up above");
            table.set_(&mut store, j, r)?;
        }

        Ok(())
    }

    /// Drop an element.
    pub(crate) fn elem_drop(
        self: Pin<&mut Self>,
        gc_store: Option<&mut GcStore>,
        elem_index: ElemIndex,
    ) -> Result<(), OutOfMemory> {
        // https://webassembly.github.io/reference-types/core/exec/instructions.html#exec-elem-drop

        let Some(passive_index) = self
            .env_module()
            .passive_elements_map
            .get(&elem_index)
            .copied()
        else {
            // Note: dropping a non-passive segment is a no-op (not a trap).
            return Ok(());
        };

        self.passive_elements_mut()[passive_index.index()].clear(gc_store);
        Ok(())
    }

    /// Get a locally-defined memory.
    pub fn get_defined_memory_mut(self: Pin<&mut Self>, index: DefinedMemoryIndex) -> &mut Memory {
        &mut self.memories_mut()[index].1
    }

    /// Get a locally-defined memory.
    pub fn get_defined_memory(&self, index: DefinedMemoryIndex) -> &Memory {
        &self.memories[index].1
    }

    pub fn get_defined_memory_vmimport(&self, index: DefinedMemoryIndex) -> VMMemoryImport {
        // AN-encoding: hand the importing instance a pointer to *our*
        // enc-base slot so its JIT'd stores mirror through our shadow even
        // after `memory.grow` swaps the buffer (the slot address is stable;
        // its contents are not). `None` when no shadow exists (AN off or
        // shared memory).
        let an_enc_base_slot = if self.an_has_shadow(index) {
            let offset = self.runtime_info.offsets().vmctx_an_enc_memory_base(index);
            // SAFETY: the offset is in-bounds of this instance's `VMContext`
            // allocation by construction (`VMOffsets` sized it).
            let slot = unsafe { self.vmctx().byte_add(usize::try_from(offset).unwrap()) };
            Some(VmPtr::from(slot.cast::<u8>()))
        } else {
            None
        };
        crate::runtime::vm::VMMemoryImport {
            from: self.memory_ptr(index).into(),
            vmctx: self.vmctx().into(),
            index,
            an_enc_base_slot,
        }
    }

    /// Do a `memory.copy`
    ///
    /// # Errors
    ///
    /// Returns a `Trap` error when the source or destination ranges are out of
    /// bounds.
    pub(crate) fn memory_copy(
        self: Pin<&mut Self>,
        dst_index: MemoryIndex,
        dst: u64,
        src_index: MemoryIndex,
        src: u64,
        len: u64,
    ) -> Result<(), Trap> {
        // https://webassembly.github.io/reference-types/core/exec/instructions.html#exec-memory-copy

        let src_mem = self.get_memory(src_index);
        let dst_mem = self.get_memory(dst_index);

        let src = self.validate_inbounds(src_mem.current_length(), src, len)?;
        let dst = self.validate_inbounds(dst_mem.current_length(), dst, len)?;
        let len = usize::try_from(len).unwrap();

        // Bounds and casts are checked above, by this point we know that
        // everything is safe.
        unsafe {
            let dst = dst_mem.base.as_ptr().add(dst);
            let src = src_mem.base.as_ptr().add(src);
            // FIXME audit whether this is safe in the presence of shared memory
            // (https://github.com/bytecodealliance/wasmtime/issues/4203).
            ptr::copy(src, dst, len);
        }

        Ok(())
    }

    fn validate_inbounds(&self, max: usize, ptr: u64, len: u64) -> Result<usize, Trap> {
        let oob = || Trap::MemoryOutOfBounds;
        let end = ptr
            .checked_add(len)
            .and_then(|i| usize::try_from(i).ok())
            .ok_or_else(oob)?;
        if end > max {
            Err(oob())
        } else {
            Ok(ptr.try_into().unwrap())
        }
    }

    /// Perform the `memory.fill` operation on a locally defined memory.
    ///
    /// # Errors
    ///
    /// Returns a `Trap` error if the memory range is out of bounds.
    pub(crate) fn memory_fill(
        self: Pin<&mut Self>,
        memory_index: DefinedMemoryIndex,
        dst: u64,
        val: u8,
        len: u64,
    ) -> Result<(), Trap> {
        let memory_index = self.env_module().memory_index(memory_index);
        let memory = self.get_memory(memory_index);
        let dst = self.validate_inbounds(memory.current_length(), dst, len)?;
        let len = usize::try_from(len).unwrap();

        // Bounds and casts are checked above, by this point we know that
        // everything is safe.
        unsafe {
            let dst = memory.base.as_ptr().add(dst);
            // FIXME audit whether this is safe in the presence of shared memory
            // (https://github.com/bytecodealliance/wasmtime/issues/4203).
            ptr::write_bytes(dst, val, len);
        }

        Ok(())
    }

    /// Get the internal storage range of a particular Wasm data segment.
    pub(crate) fn wasm_data_range(&self, index: DataIndex) -> Range<u32> {
        match self.env_module().passive_data_map.get(&index) {
            Some(range) if !self.dropped_data.contains(index) => range.clone(),
            _ => 0..0,
        }
    }

    /// Given an internal storage range of a Wasm data segment (or subset of a
    /// Wasm data segment), get the data's raw bytes.
    pub(crate) fn wasm_data(&self, range: Range<u32>) -> &[u8] {
        let start = usize::try_from(range.start).unwrap();
        let end = usize::try_from(range.end).unwrap();
        &self.runtime_info.wasm_data()[start..end]
    }

    /// Performs the `memory.init` operation.
    ///
    /// # Errors
    ///
    /// Returns a `Trap` error if the destination range is out of this module's
    /// memory's bounds or if the source range is outside the data segment's
    /// bounds.
    pub(crate) fn memory_init(
        self: Pin<&mut Self>,
        memory_index: MemoryIndex,
        data_index: DataIndex,
        dst: u64,
        src: u32,
        len: u32,
    ) -> Result<(), Trap> {
        let range = self.wasm_data_range(data_index);
        self.memory_init_segment(memory_index, range, dst, src, len)
    }

    pub(crate) fn memory_init_segment(
        self: Pin<&mut Self>,
        memory_index: MemoryIndex,
        range: Range<u32>,
        dst: u64,
        src: u32,
        len: u32,
    ) -> Result<(), Trap> {
        // https://webassembly.github.io/bulk-memory-operations/core/exec/instructions.html#exec-memory-init

        let memory = self.get_memory(memory_index);
        let data = self.wasm_data(range);
        let dst = self.validate_inbounds(memory.current_length(), dst, len.into())?;
        let src = self.validate_inbounds(data.len(), src.into(), len.into())?;
        let len = len as usize;

        unsafe {
            let src_start = data.as_ptr().add(src);
            let dst_start = memory.base.as_ptr().add(dst);
            // FIXME audit whether this is safe in the presence of shared memory
            // (https://github.com/bytecodealliance/wasmtime/issues/4203).
            ptr::copy_nonoverlapping(src_start, dst_start, len);
        }

        Ok(())
    }

    /// Drop the given data segment, truncating its length to zero.
    pub(crate) fn data_drop(
        self: Pin<&mut Self>,
        data_index: DataIndex,
    ) -> Result<(), OutOfMemory> {
        self.dropped_data_mut().insert(data_index)?;

        // Note that we don't check that we actually removed a segment because
        // dropping a non-passive segment is a no-op (not a trap).

        Ok(())
    }

    /// Get a table by index regardless of whether it is locally-defined
    /// or an imported, foreign table. Ensure that the given range of
    /// elements in the table is lazily initialized.  We define this
    /// operation all-in-one for safety, to ensure the lazy-init
    /// happens.
    ///
    /// Takes an `Iterator` for the index-range to lazy-initialize,
    /// for flexibility. This can be a range, single item, or empty
    /// sequence, for example. The iterator should return indices in
    /// increasing order, so that the break-at-out-of-bounds behavior
    /// works correctly.
    pub(crate) fn get_table_with_lazy_init(
        self: Pin<&mut Self>,
        registry: &ModuleRegistry,
        table_index: TableIndex,
        range: impl Iterator<Item = u64>,
    ) -> &mut Table {
        let (idx, instance) = self.defined_table_index_and_instance(table_index);
        instance.get_defined_table_with_lazy_init(registry, idx, range)
    }

    /// Gets the raw runtime table data structure owned by this instance
    /// given the provided `idx`.
    ///
    /// The `range` specified is eagerly initialized for funcref tables.
    pub fn get_defined_table_with_lazy_init(
        mut self: Pin<&mut Self>,
        registry: &ModuleRegistry,
        idx: DefinedTableIndex,
        range: impl IntoIterator<Item = u64>,
    ) -> &mut Table {
        let elt_ty = self.tables[idx].1.element_type();

        if elt_ty == TableElementType::Func {
            for i in range {
                match self.tables[idx].1.get_func_maybe_init(i) {
                    // Uninitialized table element.
                    Ok(None) => {}
                    // Initialized table element, move on to the next.
                    Ok(Some(_)) => continue,
                    // Out-of-bounds; caller will handle by likely
                    // throwing a trap. No work to do to lazy-init
                    // beyond the end.
                    Err(_) => break,
                };

                // The table element `i` is uninitialized and is now being
                // initialized. This must imply that a `precompiled` list of
                // function indices is available for this table. The precompiled
                // list is extracted and then it is consulted with `i` to
                // determine the function that is going to be initialized. Note
                // that `i` may be outside the limits of the static
                // initialization so it's a fallible `get` instead of an index.
                let module = self.env_module();
                let precomputed = match &module.table_initialization.initial_values[idx] {
                    TableInitialValue::Null { precomputed } => precomputed,
                    TableInitialValue::Expr(_) => unreachable!(),
                };
                // Panicking here helps catch bugs rather than silently truncating by accident.
                let func_index = precomputed.get(usize::try_from(i).unwrap()).cloned();
                let func_ref = func_index
                    .and_then(|func_index| self.as_mut().get_func_ref(registry, func_index));
                self.as_mut().tables_mut()[idx]
                    .1
                    .set_func(i, func_ref)
                    .expect("Table type should match and index should be in-bounds");
            }
        }

        self.get_defined_table(idx)
    }

    /// Get a table by index regardless of whether it is locally-defined or an
    /// imported, foreign table.
    pub(crate) fn get_table(self: Pin<&mut Self>, table_index: TableIndex) -> &mut Table {
        let (idx, instance) = self.defined_table_index_and_instance(table_index);
        instance.get_defined_table(idx)
    }

    /// Get a locally-defined table.
    pub(crate) fn get_defined_table(self: Pin<&mut Self>, index: DefinedTableIndex) -> &mut Table {
        &mut self.tables_mut()[index].1
    }

    pub(crate) fn defined_table_index_and_instance<'a>(
        self: Pin<&'a mut Self>,
        index: TableIndex,
    ) -> (DefinedTableIndex, Pin<&'a mut Instance>) {
        if let Some(defined_table_index) = self.env_module().defined_table_index(index) {
            (defined_table_index, self)
        } else {
            let import = self.imported_table(index);
            let index = import.index;
            let vmctx = import.vmctx.as_non_null();
            // SAFETY: the validity of `self` means that the reachable instances
            // should also all be owned by the same store and fully initialized,
            // so it's safe to laterally move from a mutable borrow of this
            // instance to a mutable borrow of a sibling instance.
            let foreign_instance = unsafe { self.sibling_vmctx_mut(vmctx) };
            (index, foreign_instance)
        }
    }

    /// Same as `self.runtime_info.env_module()` but additionally returns the
    /// `Pin<&mut Self>` with the same original lifetime.
    pub fn module_and_self(self: Pin<&mut Self>) -> (&wasmtime_environ::Module, Pin<&mut Self>) {
        // SAFETY: this function is projecting both `&Module` and the same
        // pointer both connected to the same lifetime. This is safe because
        // it's a contract of `Pin<&mut Self>` that the `runtime_info` field is
        // never written, meaning it's effectively unsafe to have `&mut Module`
        // projected from `Pin<&mut Self>`. Consequently it's safe to have a
        // read-only view of the field while still retaining mutable access to
        // all other fields.
        let module = self.runtime_info.env_module();
        let module = &raw const *module;
        let module = unsafe { &*module };
        (module, self)
    }

    /// Initialize the VMContext data associated with this Instance.
    ///
    /// The `VMContext` memory is assumed to be uninitialized; any field
    /// that we need in a certain state will be explicitly written by this
    /// function.
    unsafe fn initialize_vmctx(self: Pin<&mut Self>, store: &StoreOpaque, imports: Imports) {
        let (module, mut instance) = self.module_and_self();

        // SAFETY: the type of the magic field is indeed `u32` and this function
        // is initializing its value.
        unsafe {
            let offsets = instance.runtime_info.offsets();
            instance
                .vmctx_plus_offset_raw::<u32>(offsets.ptr.vmctx_magic())
                .write(VMCONTEXT_MAGIC);
        }

        // SAFETY: it's up to the caller to provide a valid store pointer here.
        unsafe {
            instance.as_mut().set_store(store);
        }

        // Initialize shared types
        //
        // SAFETY: validity of the vmctx means it should be safe to write to it
        // here.
        unsafe {
            let types = NonNull::from(instance.runtime_info.type_ids());
            instance.type_ids_array().write(types.cast().into());
        }

        // Initialize the built-in functions
        //
        // SAFETY: the type of the builtin functions field is indeed a pointer
        // and the pointer being filled in here, plus the vmctx is valid to
        // write to during initialization.
        unsafe {
            static BUILTINS: VMBuiltinFunctionsArray = VMBuiltinFunctionsArray::INIT;
            let ptr = BUILTINS.expose_provenance();
            let offsets = instance.runtime_info.offsets();
            instance
                .vmctx_plus_offset_raw(offsets.ptr.vmctx_builtin_functions())
                .write(VmPtr::from(ptr));
        }

        // Initialize the imports
        //
        // SAFETY: the vmctx is safe to initialize during this function and
        // validity of each item itself is a contract the caller must uphold.
        debug_assert_eq!(imports.functions.len(), module.num_imported_funcs);
        unsafe {
            let offsets = instance.runtime_info.offsets();
            ptr::copy_nonoverlapping(
                imports.functions.as_ptr(),
                instance
                    .vmctx_plus_offset_raw(offsets.vmctx_imported_functions_begin())
                    .as_ptr(),
                imports.functions.len(),
            );
            debug_assert_eq!(imports.tables.len(), module.num_imported_tables);
            ptr::copy_nonoverlapping(
                imports.tables.as_ptr(),
                instance
                    .vmctx_plus_offset_raw(offsets.vmctx_imported_tables_begin())
                    .as_ptr(),
                imports.tables.len(),
            );
            debug_assert_eq!(imports.memories.len(), module.num_imported_memories);
            ptr::copy_nonoverlapping(
                imports.memories.as_ptr(),
                instance
                    .vmctx_plus_offset_raw(offsets.vmctx_imported_memories_begin())
                    .as_ptr(),
                imports.memories.len(),
            );
            debug_assert_eq!(imports.globals.len(), module.num_imported_globals);
            ptr::copy_nonoverlapping(
                imports.globals.as_ptr(),
                instance
                    .vmctx_plus_offset_raw(offsets.vmctx_imported_globals_begin())
                    .as_ptr(),
                imports.globals.len(),
            );
            debug_assert_eq!(imports.tags.len(), module.num_imported_tags);
            ptr::copy_nonoverlapping(
                imports.tags.as_ptr(),
                instance
                    .vmctx_plus_offset_raw(offsets.vmctx_imported_tags_begin())
                    .as_ptr(),
                imports.tags.len(),
            );
        }

        // N.B.: there is no need to initialize the funcrefs array because we
        // eagerly construct each element in it whenever asked for a reference
        // to that element. In other words, there is no state needed to track
        // the lazy-init, so we don't need to initialize any state now.

        // Initialize the defined tables
        //
        // SAFETY: it's safe to initialize these tables during initialization
        // here and the various types of pointers and such here should all be
        // valid.
        unsafe {
            let offsets = instance.runtime_info.offsets();
            let mut ptr = instance.vmctx_plus_offset_raw(offsets.vmctx_tables_begin());
            let tables = instance.as_mut().tables_mut();
            for i in 0..module.num_defined_tables() {
                ptr.write(tables[DefinedTableIndex::new(i)].1.vmtable());
                ptr = ptr.add(1);
            }
        }

        // Initialize the defined memories. This fills in both the
        // `defined_memories` table and the `owned_memories` table at the same
        // time. Entries in `defined_memories` hold a pointer to a definition
        // (all memories) whereas the `owned_memories` hold the actual
        // definitions of memories owned (not shared) in the module.
        //
        // SAFETY: it's safe to initialize these memories during initialization
        // here and the various types of pointers and such here should all be
        // valid.
        unsafe {
            let offsets = instance.runtime_info.offsets();
            let mut ptr = instance.vmctx_plus_offset_raw(offsets.vmctx_memories_begin());
            let mut owned_ptr =
                instance.vmctx_plus_offset_raw(offsets.vmctx_owned_memories_begin());
            let memories = instance.as_mut().memories_mut();
            for i in 0..module.num_defined_memories() {
                let defined_memory_index = DefinedMemoryIndex::new(i);
                let memory_index = module.memory_index(defined_memory_index);
                if module.memories[memory_index].shared {
                    let def_ptr = memories[defined_memory_index]
                        .1
                        .as_shared_memory()
                        .unwrap()
                        .vmmemory_ptr();
                    ptr.write(VmPtr::from(def_ptr));
                } else {
                    owned_ptr.write(memories[defined_memory_index].1.vmmemory());
                    ptr.write(VmPtr::from(owned_ptr));
                    owned_ptr = owned_ptr.add(1);
                }
                ptr = ptr.add(1);
            }
        }

        // AN-encoding shadow base pointer slots are populated by
        // `Instance::set_an_enc_shadows` (invoked from `set_store` higher up
        // in this function). When AN-encoding is off the slots stay
        // uninitialized and JIT code never reads them, so no explicit
        // zero-fill is required here. See `set_an_enc_shadows` for the
        // (deliberately raw-pointer) write that initializes each slot
        // without first forming a reference to uninitialized memory.

        // Zero-initialize the globals so that nothing is uninitialized memory
        // after this function returns. The globals are actually initialized
        // with their const expression initializers after the instance is fully
        // allocated.
        //
        // SAFETY: it's safe to initialize globals during initialization
        // here. Note that while the value being written is not valid for all
        // types of globals it's initializing the memory to zero instead of
        // being in an undefined state. So it's still unsafe to access globals
        // after this, but if it's read then it'd hopefully crash faster than
        // leaving this undefined.
        unsafe {
            for (index, _init) in module.global_initializers.iter() {
                instance.global_ptr(index).write(VMGlobalDefinition::new());
            }
        }

        // Initialize the defined tags
        //
        // SAFETY: it's safe to initialize these tags during initialization
        // here and the various types of pointers and such here should all be
        // valid.
        unsafe {
            let offsets = instance.runtime_info.offsets();
            let mut ptr = instance.vmctx_plus_offset_raw(offsets.vmctx_tags_begin());
            for i in 0..module.num_defined_tags() {
                let defined_index = DefinedTagIndex::new(i);
                let tag_index = module.tag_index(defined_index);
                let tag = module.tags[tag_index];
                ptr.write(VMTagDefinition::new(
                    tag.signature.unwrap_engine_type_index(),
                ));
                ptr = ptr.add(1);
            }
        }
    }

    /// Attempts to convert from the host `addr` specified to a WebAssembly
    /// based address recorded in `WasmFault`.
    ///
    /// This method will check all linear memories that this instance contains
    /// to see if any of them contain `addr`. If one does then `Some` is
    /// returned with metadata about the wasm fault. Otherwise `None` is
    /// returned and `addr` doesn't belong to this instance.
    pub fn wasm_fault(&self, addr: usize) -> Option<WasmFault> {
        let mut fault = None;
        for (_, (_, memory)) in self.memories.iter() {
            let accessible = memory.wasm_accessible();
            if accessible.start <= addr && addr < accessible.end {
                // All linear memories should be disjoint so assert that no
                // prior fault has been found.
                assert!(fault.is_none());
                fault = Some(WasmFault {
                    memory_size: memory.byte_size(),
                    wasm_address: u64::try_from(addr - accessible.start).unwrap(),
                });
            }
        }
        fault
    }

    /// Returns the id, within this instance's store, that it's assigned.
    pub fn id(&self) -> InstanceId {
        self.id
    }

    /// Get all memories within this instance.
    ///
    /// Returns both import and defined memories.
    ///
    /// Returns both exported and non-exported memories.
    ///
    /// Gives access to the full memories space.
    pub fn all_memories(
        &self,
        store: StoreId,
    ) -> impl ExactSizeIterator<Item = (MemoryIndex, ExportMemory)> + '_ {
        self.env_module()
            .memories
            .iter()
            .map(move |(i, _)| (i, self.get_exported_memory(store, i)))
    }

    /// Return the memories defined in this instance (not imported).
    pub fn defined_memories<'a>(
        &'a self,
        store: StoreId,
    ) -> impl ExactSizeIterator<Item = ExportMemory> + 'a {
        let num_imported = self.env_module().num_imported_memories;
        self.all_memories(store)
            .skip(num_imported)
            .map(|(_i, memory)| memory)
    }

    /// Lookup an item with the given index.
    ///
    /// # Panics
    ///
    /// Panics if `export` is not valid for this instance.
    ///
    /// # Safety
    ///
    /// This function requires that `store` is the correct store which owns this
    /// instance.
    pub unsafe fn get_export_by_index_mut(
        self: Pin<&mut Self>,
        registry: &ModuleRegistry,
        store: StoreId,
        export: EntityIndex,
    ) -> Export {
        match export {
            // SAFETY: the contract of `store` owning the this instance is a
            // safety requirement of this function itself.
            EntityIndex::Function(i) => {
                Export::Function(unsafe { self.get_exported_func(registry, store, i) })
            }
            EntityIndex::Global(i) => Export::Global(self.get_exported_global(store, i)),
            EntityIndex::Table(i) => Export::Table(self.get_exported_table(store, i)),
            EntityIndex::Memory(i) => match self.get_exported_memory(store, i) {
                ExportMemory::Unshared(m) => Export::Memory(m),
                ExportMemory::Shared(m, i) => Export::SharedMemory(m, i),
            },
            EntityIndex::Tag(i) => Export::Tag(self.get_exported_tag(store, i)),
        }
    }

    fn store_mut(self: Pin<&mut Self>) -> &mut Option<VMStoreRawPtr> {
        // SAFETY: this is a pin-projection to get a mutable reference to an
        // internal field and is safe so long as the `&mut Self` temporarily
        // created is not overwritten, which it isn't here.
        unsafe { &mut self.get_unchecked_mut().store }
    }

    fn dropped_data_mut(self: Pin<&mut Self>) -> &mut TryEntitySet<DataIndex> {
        // SAFETY: see `store_mut` above.
        unsafe { &mut self.get_unchecked_mut().dropped_data }
    }

    fn memories_mut(
        self: Pin<&mut Self>,
    ) -> &mut TryPrimaryMap<DefinedMemoryIndex, (MemoryAllocationIndex, Memory)> {
        // SAFETY: see `store_mut` above.
        unsafe { &mut self.get_unchecked_mut().memories }
    }

    fn an_enc_shadows_mut(
        self: Pin<&mut Self>,
    ) -> &mut TryPrimaryMap<DefinedMemoryIndex, Option<Box<[u8]>>> {
        // SAFETY: see `store_mut` above.
        unsafe { &mut self.get_unchecked_mut().an_enc_shadows }
    }

    pub(crate) fn tables_mut(
        self: Pin<&mut Self>,
    ) -> &mut TryPrimaryMap<DefinedTableIndex, (TableAllocationIndex, Table)> {
        // SAFETY: see `store_mut` above.
        unsafe { &mut self.get_unchecked_mut().tables }
    }

    #[cfg(feature = "wmemcheck")]
    pub(super) fn wmemcheck_state_mut(self: Pin<&mut Self>) -> &mut Option<Wmemcheck> {
        // SAFETY: see `store_mut` above.
        unsafe { &mut self.get_unchecked_mut().wmemcheck_state }
    }
}

// SAFETY: `layout` should describe this accurately and `OwnedVMContext` is the
// last field of `ComponentInstance`.
unsafe impl InstanceLayout for Instance {
    const INIT_ZEROED: bool = false;
    type VMContext = VMContext;

    fn layout(&self) -> Layout {
        Self::alloc_layout(self.runtime_info.offsets())
    }

    fn owned_vmctx(&self) -> &OwnedVMContext<VMContext> {
        &self.vmctx
    }

    fn owned_vmctx_mut(&mut self) -> &mut OwnedVMContext<VMContext> {
        &mut self.vmctx
    }
}

pub type InstanceHandle = OwnedInstance<Instance>;

/// A handle holding an `Instance` of a WebAssembly module.
///
/// This structure is an owning handle of the `instance` contained internally.
/// When this value goes out of scope it will deallocate the `Instance` and all
/// memory associated with it.
///
/// Note that this lives within a `StoreOpaque` on a list of instances that a
/// store is keeping alive.
#[derive(Debug)]
#[repr(transparent)] // guarantee this is a zero-cost wrapper
pub struct OwnedInstance<T: InstanceLayout> {
    /// The raw pointer to the instance that was allocated.
    ///
    /// Note that this is not equivalent to `Box<Instance>` because the
    /// allocation here has a `VMContext` trailing after it. Thus the custom
    /// destructor to invoke the `dealloc` function with the appropriate
    /// layout.
    instance: SendSyncPtr<T>,
    _marker: marker::PhantomData<Box<(T, OwnedVMContext<T::VMContext>)>>,
}

/// Structure that must be placed at the end of a type implementing
/// `InstanceLayout`.
#[repr(align(16))] // match the alignment of VMContext
pub struct OwnedVMContext<T> {
    /// A pointer to the `vmctx` field at the end of the `structure`.
    ///
    /// If you're looking at this a reasonable question would be "why do we need
    /// a pointer to ourselves?" because after all the pointer's value is
    /// trivially derivable from any `&Instance` pointer. The rationale for this
    /// field's existence is subtle, but it's required for correctness. The
    /// short version is "this makes miri happy".
    ///
    /// The long version of why this field exists is that the rules that MIRI
    /// uses to ensure pointers are used correctly have various conditions on
    /// them depend on how pointers are used. More specifically if `*mut T` is
    /// derived from `&mut T`, then that invalidates all prior pointers derived
    /// from the `&mut T`. This means that while we liberally want to re-acquire
    /// a `*mut VMContext` throughout the implementation of `Instance` the
    /// trivial way, a function `fn vmctx(Pin<&mut Instance>) -> *mut VMContext`
    /// would effectively invalidate all prior `*mut VMContext` pointers
    /// acquired. The purpose of this field is to serve as a sort of
    /// source-of-truth for where `*mut VMContext` pointers come from.
    ///
    /// This field is initialized when the `Instance` is created with the
    /// original allocation's pointer. That means that the provenance of this
    /// pointer contains the entire allocation (both instance and `VMContext`).
    /// This provenance bit is then "carried through" where `fn vmctx` will base
    /// all returned pointers on this pointer itself. This provides the means of
    /// never invalidating this pointer throughout MIRI and additionally being
    /// able to still temporarily have `Pin<&mut Instance>` methods and such.
    ///
    /// It's important to note, though, that this is not here purely for MIRI.
    /// The careful construction of the `fn vmctx` method has ramifications on
    /// the LLVM IR generated, for example. A historical CVE on Wasmtime,
    /// GHSA-ch89-5g45-qwc7, was caused due to relying on undefined behavior. By
    /// deriving VMContext pointers from this pointer it specifically hints to
    /// LLVM that trickery is afoot and it properly informs `noalias` and such
    /// annotations and analysis. More-or-less this pointer is actually loaded
    /// in LLVM IR which helps defeat otherwise present aliasing optimizations,
    /// which we want, since writes to this should basically never be optimized
    /// out.
    ///
    /// As a final note it's worth pointing out that the machine code generated
    /// for accessing `fn vmctx` is still as one would expect. This member isn't
    /// actually ever loaded at runtime (or at least shouldn't be). Perhaps in
    /// the future if the memory consumption of this field is a problem we could
    /// shrink it slightly, but for now one extra pointer per wasm instance
    /// seems not too bad.
    vmctx_self_reference: SendSyncPtr<T>,

    /// This field ensures that going from `Pin<&mut T>` to `&mut T` is not a
    /// safe operation.
    _marker: core::marker::PhantomPinned,
}

impl<T> OwnedVMContext<T> {
    /// Creates a new blank vmctx to place at the end of an instance.
    pub fn new() -> OwnedVMContext<T> {
        OwnedVMContext {
            vmctx_self_reference: SendSyncPtr::new(NonNull::dangling()),
            _marker: core::marker::PhantomPinned,
        }
    }
}

/// Helper trait to plumb both core instances and component instances into
/// `OwnedInstance` below.
///
/// # Safety
///
/// This trait requires `layout` to correctly describe `Self` and appropriately
/// allocate space for `Self::VMContext` afterwards. Additionally the field
/// returned by `owned_vmctx()` must be the last field in the structure.
pub unsafe trait InstanceLayout {
    /// Whether or not to allocate this instance with `alloc_zeroed` or `alloc`.
    const INIT_ZEROED: bool;

    /// The trailing `VMContext` type at the end of this instance.
    type VMContext;

    /// The memory layout to use to allocate and deallocate this instance.
    fn layout(&self) -> Layout;

    fn owned_vmctx(&self) -> &OwnedVMContext<Self::VMContext>;
    fn owned_vmctx_mut(&mut self) -> &mut OwnedVMContext<Self::VMContext>;

    /// Returns the `vmctx_self_reference` set above.
    #[inline]
    fn vmctx(&self) -> NonNull<Self::VMContext> {
        // The definition of this method is subtle but intentional. The goal
        // here is that effectively this should return `&mut self.vmctx`, but
        // it's not quite so simple. Some more documentation is available on the
        // `vmctx_self_reference` field, but the general idea is that we're
        // creating a pointer to return with proper provenance. Provenance is
        // still in the works in Rust at the time of this writing but the load
        // of the `self.vmctx_self_reference` field is important here as it
        // affects how LLVM thinks about aliasing with respect to the returned
        // pointer.
        //
        // The intention of this method is to codegen to machine code as `&mut
        // self.vmctx`, however. While it doesn't show up like this in LLVM IR
        // (there's an actual load of the field) it does look like that by the
        // time the backend runs. (that's magic to me, the backend removing
        // loads...)
        let owned_vmctx = self.owned_vmctx();
        let owned_vmctx_raw = NonNull::from(owned_vmctx);
        // SAFETY: it's part of the contract of `InstanceLayout` and the usage
        // with `OwnedInstance` that this indeed points to the vmctx.
        let addr = unsafe { owned_vmctx_raw.add(1) };
        owned_vmctx
            .vmctx_self_reference
            .as_non_null()
            .with_addr(addr.addr())
    }

    /// Helper function to access various locations offset from our `*mut
    /// VMContext` object.
    ///
    /// Note that this method takes `&self` as an argument but returns
    /// `NonNull<T>` which is frequently used to mutate said memory. This is an
    /// intentional design decision where the safety of the modification of
    /// memory is placed as a burden onto the caller. The implementation of this
    /// method explicitly does not require `&mut self` to acquire mutable
    /// provenance to update the `VMContext` region. Instead all pointers into
    /// the `VMContext` area have provenance/permissions to write.
    ///
    /// Also note though that care must be taken to ensure that reads/writes of
    /// memory must only happen where appropriate, for example a non-atomic
    /// write (as most are) should never happen concurrently with another read
    /// or write. It's generally on the burden of the caller to adhere to this.
    ///
    /// Also of note is that most of the time the usage of this method falls
    /// into one of:
    ///
    /// * Something in the VMContext is being read or written. In that case use
    ///   `vmctx_plus_offset` or `vmctx_plus_offset_mut` if possible due to
    ///   that having a safer lifetime.
    ///
    /// * A pointer is being created to pass to other VM* data structures. In
    ///   that situation the lifetime of all VM data structures are typically
    ///   tied to the `Store<T>` which is what provides the guarantees around
    ///   concurrency/etc.
    ///
    /// There's quite a lot of unsafety riding on this method, especially
    /// related to the ascription `T` of the byte `offset`. It's hoped that in
    /// the future we're able to settle on an in theory safer design.
    ///
    /// # Safety
    ///
    /// This method is unsafe because the `offset` must be within bounds of the
    /// `VMContext` object trailing this instance. Additionally `T` must be a
    /// valid ascription of the value that resides at that location.
    unsafe fn vmctx_plus_offset_raw<T: VmSafe>(&self, offset: impl Into<u32>) -> NonNull<T> {
        // SAFETY: the safety requirements of `byte_add` are forwarded to this
        // method's caller.
        unsafe {
            self.vmctx()
                .byte_add(usize::try_from(offset.into()).unwrap())
                .cast()
        }
    }

    /// Helper above `vmctx_plus_offset_raw` which transfers the lifetime of
    /// `&self` to the returned reference `&T`.
    ///
    /// # Safety
    ///
    /// See the safety documentation of `vmctx_plus_offset_raw`.
    unsafe fn vmctx_plus_offset<T: VmSafe>(&self, offset: impl Into<u32>) -> &T {
        // SAFETY: this method has the same safety requirements as
        // `vmctx_plus_offset_raw`.
        unsafe { self.vmctx_plus_offset_raw(offset).as_ref() }
    }

    /// Helper above `vmctx_plus_offset_raw` which transfers the lifetime of
    /// `&mut self` to the returned reference `&mut T`.
    ///
    /// # Safety
    ///
    /// See the safety documentation of `vmctx_plus_offset_raw`.
    unsafe fn vmctx_plus_offset_mut<T: VmSafe>(
        self: Pin<&mut Self>,
        offset: impl Into<u32>,
    ) -> &mut T {
        // SAFETY: this method has the same safety requirements as
        // `vmctx_plus_offset_raw`.
        unsafe { self.vmctx_plus_offset_raw(offset).as_mut() }
    }
}

impl<T: InstanceLayout> OwnedInstance<T> {
    /// Allocates a new `OwnedInstance` and places `instance` inside of it.
    ///
    /// This will `instance`
    pub(super) fn new(mut instance: T) -> Result<OwnedInstance<T>, OutOfMemory> {
        let layout = instance.layout();
        debug_assert!(layout.size() >= size_of_val(&instance));
        debug_assert!(layout.align() >= align_of_val(&instance));

        // SAFETY: it's up to us to assert that `layout` has a non-zero size,
        // which is asserted here.
        let ptr = unsafe {
            assert!(layout.size() > 0);
            if T::INIT_ZEROED {
                alloc::alloc::alloc_zeroed(layout)
            } else {
                alloc::alloc::alloc(layout)
            }
        };
        let Some(instance_ptr) = NonNull::new(ptr.cast::<T>()) else {
            return Err(OutOfMemory::new(layout.size()));
        };

        // SAFETY: it's part of the unsafe contract of `InstanceLayout` that the
        // `add` here is appropriate for the layout allocated.
        let vmctx_self_reference = unsafe { instance_ptr.add(1).cast() };
        instance.owned_vmctx_mut().vmctx_self_reference = vmctx_self_reference.into();

        // SAFETY: we allocated above and it's an unsafe contract of
        // `InstanceLayout` that the layout is suitable for writing the
        // instance.
        unsafe {
            instance_ptr.write(instance);
        }

        let ret = OwnedInstance {
            instance: SendSyncPtr::new(instance_ptr),
            _marker: marker::PhantomData,
        };

        // Double-check various vmctx calculations are correct.
        debug_assert_eq!(
            vmctx_self_reference.addr(),
            // SAFETY: `InstanceLayout` should guarantee it's safe to add 1 to
            // the last field to get a pointer to 1-byte-past-the-end of an
            // object, which should be valid.
            unsafe { NonNull::from(ret.get().owned_vmctx()).add(1).addr() }
        );
        debug_assert_eq!(vmctx_self_reference.addr(), ret.get().vmctx().addr());

        Ok(ret)
    }

    /// Gets the raw underlying `&Instance` from this handle.
    pub fn get(&self) -> &T {
        // SAFETY: this is an owned instance handle that retains exclusive
        // ownership of the `Instance` inside. With `&self` given we know
        // this pointer is valid valid and the returned lifetime is connected
        // to `self` so that should also be valid.
        unsafe { self.instance.as_non_null().as_ref() }
    }

    /// Same as [`Self::get`] except for mutability.
    pub fn get_mut(&mut self) -> Pin<&mut T> {
        // SAFETY: The lifetime concerns here are the same as `get` above.
        // Otherwise `new_unchecked` is used here to uphold the contract that
        // instances are always pinned in memory.
        unsafe { Pin::new_unchecked(self.instance.as_non_null().as_mut()) }
    }
}

impl<T: InstanceLayout> Drop for OwnedInstance<T> {
    fn drop(&mut self) {
        unsafe {
            let layout = self.get().layout();
            ptr::drop_in_place(self.instance.as_ptr());
            alloc::alloc::dealloc(self.instance.as_ptr().cast(), layout);
        }
    }
}

#[derive(Debug)]
pub(crate) struct PassiveElementSegment {
    needs_gc_rooting: bool,
    elements: TryVec<ValRaw>,
}

impl PassiveElementSegment {
    /// Create a new passive element segment with the given capacity.
    pub(crate) fn new(ty: WasmRefType, capacity: usize) -> Result<Self, OutOfMemory> {
        Ok(Self {
            needs_gc_rooting: ty.is_vmgcref_type_and_not_i31(),
            elements: TryVec::with_capacity(capacity)?,
        })
    }

    /// Push a value onto this passive element segment.
    ///
    /// NB: Does not type check the value, relies on callers to ensure the
    /// value is of the correct type (generally, due to validation).
    pub(crate) fn push(&mut self, store: &mut StoreOpaque, val: Val) -> Result<()> {
        let mut val = {
            let mut store = AutoAssertNoGc::new(store);
            val.to_raw_(&mut store)?
        };
        if self.needs_gc_rooting {
            // Note that `anyref` accessors and constructors are used here
            // without actually checking the type of this segment or value. The
            // representation and handling of all three is the same which means
            // that this should work out.
            let gc_ref = val.get_anyref();
            debug_assert_eq!(gc_ref, val.get_exnref());
            debug_assert_eq!(gc_ref, val.get_externref());
            if let Some(gc_ref) = VMGcRef::from_raw_u32(gc_ref) {
                if let Some(gc_store) = store.optional_gc_store_mut() {
                    val = ValRaw::anyref(gc_store.clone_gc_ref(&gc_ref).as_raw_u32());
                }
            }
        }
        self.elements.push(val)?;
        Ok(())
    }

    /// Clear this segment's elements.
    pub(crate) fn clear(&mut self, mut gc_store: Option<&mut GcStore>) {
        let elements = mem::take(&mut self.elements);
        if !self.needs_gc_rooting {
            return;
        }
        for val in elements {
            // Like above, `anyref` accessors are used here even if this
            // element segment has a different type because all of the vmgcref
            // types are treated the same way.
            let gc_ref = val.get_anyref();
            debug_assert_eq!(gc_ref, val.get_exnref());
            debug_assert_eq!(gc_ref, val.get_externref());
            if let Some(gc_ref) = VMGcRef::from_raw_u32(gc_ref) {
                if let Some(gc_store) = gc_store.as_deref_mut() {
                    let _ = gc_store.drop_gc_ref(gc_ref);
                }
            }
        }
    }

    /// The elements of this segment.
    pub(crate) fn elements(&self) -> &[ValRaw] {
        &self.elements
    }
}
