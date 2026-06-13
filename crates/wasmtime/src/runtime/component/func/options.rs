use crate::StoreContextMut;
#[cfg(feature = "component-model-async")]
use crate::component::concurrent::ConcurrentState;
use crate::component::matching::InstanceType;
use crate::component::resources::{HostResourceData, HostResourceIndex, HostResourceTables};
use crate::component::store::ComponentTaskState;
use crate::component::{Instance, ResourceType, RuntimeInstance};
use crate::prelude::*;
use crate::runtime::vm::VMFuncRef;
use crate::runtime::vm::component::{ComponentInstance, HandleTable, ResourceTables};
use crate::store::{StoreId, StoreOpaque};
use alloc::sync::Arc;
use core::fmt;
use core::pin::Pin;
use core::ptr::NonNull;
use wasmtime_environ::component::{
    CanonicalOptions, CanonicalOptionsDataModel, ComponentTypes, OptionsIndex,
    TypeResourceTableIndex,
};

/// A helper structure which is a "package" of the context used during lowering
/// values into a component (or storing them into memory).
///
/// This type is used by the `Lower` trait extensively and contains any
/// contextual information necessary related to the context in which the
/// lowering is happening.
#[doc(hidden)]
pub struct LowerContext<'a, T: 'static> {
    /// Lowering may involve invoking memory allocation functions so part of the
    /// context here is carrying access to the entire store that wasm is
    /// executing within. This store serves as proof-of-ability to actually
    /// execute wasm safely.
    pub store: StoreContextMut<'a, T>,

    /// Lowering always happens into a function that's been `canon lift`'d or
    /// `canon lower`'d, both of which specify a set of options for the
    /// canonical ABI. For example details like string encoding are contained
    /// here along with which memory pointers are relative to or what the memory
    /// allocation function is.
    options: OptionsIndex,

    /// Lowering happens within the context of a component instance and this
    /// field stores the type information of that component instance. This is
    /// used for type lookups and general type queries during the
    /// lifting/lowering process.
    pub types: &'a ComponentTypes,

    /// Index of the component instance that's being lowered into.
    instance: Instance,

    /// Whether to allow `options.realloc` to be used when lowering.
    allow_realloc: bool,

    /// Byte ranges of guest linear memory written by host-side lowering
    /// through this context, recorded for AN-encoding shadow maintenance.
    ///
    /// Host-side lowering writes raw bytes that the JIT store-mirroring
    /// cannot see, so the encoded shadow must be re-encoded for exactly
    /// these ranges before control re-enters wasm. Ranges are recorded by
    /// [`LowerContext::get`] / [`LowerContext::slice_mut`] (exact) and
    /// [`LowerContext::as_slice_mut`] (whole memory, conservative), and
    /// drained by [`LowerContext::an_flush_dirty`].
    an_dirty: Vec<core::ops::Range<usize>>,
}

#[doc(hidden)]
impl<'a, T: 'static> LowerContext<'a, T> {
    /// Creates a new lowering context from the specified parameters.
    pub fn new(
        store: StoreContextMut<'a, T>,
        options: OptionsIndex,
        instance: Instance,
    ) -> LowerContext<'a, T> {
        // Debug-assert that if we can't block that blocking is indeed allowed.
        // This'll catch when this is accidentally created outside of a fiber
        // when we need to be on a fiber.
        if cfg!(debug_assertions) && !store.0.can_block() {
            store.0.validate_sync_call().unwrap();
        }
        let (component, store) = instance.component_and_store_mut(store.0);
        LowerContext {
            store: StoreContextMut(store),
            options,
            types: component.types(),
            instance,
            allow_realloc: true,
            an_dirty: Vec::new(),
        }
    }

    /// Like `new`, except disallows use of `options.realloc`.
    ///
    /// The returned object will panic if its `realloc` method is called.
    ///
    /// This is meant for use when lowering "flat" values (i.e. values which
    /// require no allocations) into already-allocated memory or into stack
    /// slots, in which case the lowering may safely be done outside of a fiber
    /// since there is no need to make any guest calls.
    #[cfg(feature = "component-model-async")]
    pub(crate) fn new_without_realloc(
        store: StoreContextMut<'a, T>,
        options: OptionsIndex,
        instance: Instance,
    ) -> LowerContext<'a, T> {
        let (component, store) = instance.component_and_store_mut(store.0);
        LowerContext {
            store: StoreContextMut(store),
            options,
            types: component.types(),
            instance,
            allow_realloc: false,
            an_dirty: Vec::new(),
        }
    }

    /// Returns the `&ComponentInstance` that's being lowered into.
    pub fn instance(&self) -> &ComponentInstance {
        self.instance.id().get(self.store.0)
    }

    /// Returns the `&mut ComponentInstance` that's being lowered into.
    pub fn instance_mut(&mut self) -> Pin<&mut ComponentInstance> {
        self.instance.id().get_mut(self.store.0)
    }

    /// Returns the canonical options that are being used during lifting.
    pub fn options(&self) -> &CanonicalOptions {
        &self.instance().component().env_component().options[self.options]
    }

    /// Returns a view into memory as a mutable slice of bytes.
    ///
    /// The caller may write anywhere through the returned borrow, so the
    /// whole memory is conservatively recorded as host-written for
    /// AN-encoding shadow maintenance. Lowering paths that know their exact
    /// range should use [`LowerContext::slice_mut`] or
    /// [`LowerContext::get`] instead.
    ///
    /// # Panics
    ///
    /// This will panic if memory has not been configured for this lowering
    /// (e.g. it wasn't present during the specification of canonical options).
    pub fn as_slice_mut(&mut self) -> &mut [u8] {
        self.an_record_write(0, usize::MAX);
        self.as_slice_mut_untracked()
    }

    /// Like [`LowerContext::as_slice_mut`] but records nothing for the
    /// AN-encoding shadow. For read-only uses (bounds validation) and for
    /// callers that record their exact written range themselves.
    pub(crate) fn as_slice_mut_untracked(&mut self) -> &mut [u8] {
        self.instance.options_memory_mut(self.store.0, self.options)
    }

    /// Returns a mutable view of `len` bytes of memory at `offset`,
    /// recording the range as host-written for AN-encoding shadow
    /// maintenance.
    ///
    /// # Panics
    ///
    /// Panics if `offset + len` is out of bounds (same as the slicing
    /// expression `&mut as_slice_mut()[offset..][..len]` it replaces) or if
    /// memory has not been configured for this lowering.
    pub fn slice_mut(&mut self, offset: usize, len: usize) -> &mut [u8] {
        self.an_record_write(offset, len);
        &mut self.as_slice_mut_untracked()[offset..][..len]
    }

    /// Records `[offset, offset + len)` of the configured memory as
    /// host-written, for AN-encoding shadow maintenance.
    ///
    /// Ranges coalesce with the most recently recorded range when they
    /// touch (lowering mostly writes sequentially) and the list is bounded:
    /// on overflow everything collapses into one bounding range.
    /// Over-approximation is harmless for correctness (the resync
    /// re-encodes from raw, which is idempotent).
    fn an_record_write(&mut self, offset: usize, len: usize) {
        if len == 0 {
            return;
        }
        let end = offset.saturating_add(len);
        if let Some(last) = self.an_dirty.last_mut() {
            if offset <= last.end && end >= last.start {
                last.start = last.start.min(offset);
                last.end = last.end.max(end);
                return;
            }
        }
        const MAX_DIRTY_RANGES: usize = 128;
        if self.an_dirty.len() >= MAX_DIRTY_RANGES {
            let mut start = offset;
            let mut max_end = end;
            for r in &self.an_dirty {
                start = start.min(r.start);
                max_end = max_end.max(r.end);
            }
            self.an_dirty.clear();
            self.an_dirty.push(start..max_end);
            return;
        }
        self.an_dirty.push(offset..end);
    }

    /// Re-encodes the AN-encoding shadow of the configured memory for every
    /// recorded host-written range, then clears the record.
    ///
    /// Must run before control re-enters wasm (a `realloc` call, the lifted
    /// call itself, resuming the caller after a hostcall): the boundary
    /// cross-check and the opt-in per-load validity check both compare the
    /// shadow against raw bytes, and host-side lowering writes raw bytes
    /// the JIT store-mirroring cannot see. No-op when nothing was recorded
    /// or when the memory has no AN-encoding shadow (AN-encoding off).
    ///
    /// Ranges past the end of memory are clamped by the re-encode itself,
    /// so the conservative whole-memory record (`0..usize::MAX`) from
    /// [`LowerContext::as_slice_mut`] simply re-encodes everything.
    pub(crate) fn an_flush_dirty(&mut self) {
        if self.an_dirty.is_empty() {
            return;
        }
        let ranges = core::mem::take(&mut self.an_dirty);
        let memory = match self.instance.an_options_memory(self.store.0, self.options) {
            Some(m) => m,
            None => return,
        };
        for r in ranges {
            memory.an_resync_range(&mut self.store, r.start, r.end - r.start);
        }
    }

    /// Invokes the memory allocation function (which is style after `realloc`)
    /// with the specified parameters.
    ///
    /// # Panics
    ///
    /// This will panic if realloc hasn't been configured for this lowering via
    /// its canonical options.
    pub fn realloc(
        &mut self,
        old: usize,
        old_size: usize,
        old_align: u32,
        new_size: usize,
    ) -> Result<usize> {
        assert!(self.allow_realloc);

        // Control re-enters wasm: anything lowered so far must be visible in
        // the AN-encoding shadow before guest code runs (the opt-in per-load
        // validity check reads the shadow at every i32 load).
        self.an_flush_dirty();

        let (component, store) = self.instance.component_and_store_mut(self.store.0);
        let instance = self.instance.id().get(store);
        let options = &component.env_component().options[self.options];
        let realloc_ty = component.realloc_func_ty();
        let realloc = match options.data_model {
            CanonicalOptionsDataModel::Gc {} => unreachable!(),
            CanonicalOptionsDataModel::LinearMemory(m) => m.realloc.unwrap(),
        };
        let realloc = instance.runtime_realloc(realloc);

        let params = (
            u32::try_from(old)?,
            u32::try_from(old_size)?,
            old_align,
            u32::try_from(new_size)?,
        );

        type ReallocFunc = crate::TypedFunc<(u32, u32, u32, u32), u32>;

        // Invoke the wasm malloc function using its raw and statically known
        // signature.
        let result = unsafe {
            ReallocFunc::call_raw(&mut StoreContextMut(store), &realloc_ty, realloc, params)?
        };

        if result % old_align != 0 {
            bail!("realloc return: result not aligned");
        }
        let result = usize::try_from(result)?;

        if self
            .as_slice_mut_untracked()
            .get_mut(result..)
            .and_then(|s| s.get_mut(..new_size))
            .is_none()
        {
            bail!("realloc return: beyond end of memory")
        }

        Ok(result)
    }

    /// Returns a fixed mutable slice of memory `N` bytes large starting at
    /// offset `N`, panicking on out-of-bounds.
    ///
    /// It should be previously verified that `offset` is in-bounds via
    /// bounds-checks.
    ///
    /// # Panics
    ///
    /// This will panic if memory has not been configured for this lowering
    /// (e.g. it wasn't present during the specification of canonical options).
    pub fn get<const N: usize>(&mut self, offset: usize) -> &mut [u8; N] {
        // The returned array is always a write destination (lifting reads go
        // through `LiftContext`), so record it for the AN-encoding shadow.
        self.an_record_write(offset, N);
        // FIXME: this bounds check shouldn't actually be necessary, all
        // callers of `ComponentType::store` have already performed a bounds
        // check so we're guaranteed that `offset..offset+N` is in-bounds. That
        // being said we at least should do bounds checks in debug mode and
        // it's not clear to me how to easily structure this so that it's
        // "statically obvious" the bounds check isn't necessary.
        //
        // For now I figure we can leave in this bounds check and if it becomes
        // an issue we can optimize further later, probably with judicious use
        // of `unsafe`.
        self.as_slice_mut_untracked()[offset..]
            .first_chunk_mut()
            .unwrap()
    }

    /// Lowers an `own` resource into the guest, converting the `rep` specified
    /// into a guest-local index.
    ///
    /// The `ty` provided is which table to put this into.
    pub fn guest_resource_lower_own(
        &mut self,
        ty: TypeResourceTableIndex,
        rep: u32,
    ) -> Result<u32> {
        self.resource_tables().guest_resource_lower_own(rep, ty)
    }

    /// Lowers a `borrow` resource into the guest, converting the `rep` to a
    /// guest-local index in the `ty` table specified.
    pub fn guest_resource_lower_borrow(
        &mut self,
        ty: TypeResourceTableIndex,
        rep: u32,
    ) -> Result<u32> {
        // Implement `lower_borrow`'s special case here where if a borrow is
        // inserted into a table owned by the instance which implemented the
        // original resource then no borrow tracking is employed and instead the
        // `rep` is returned "raw".
        //
        // This check is performed by comparing the owning instance of `ty`
        // against the owning instance of the resource that `ty` is working
        // with.
        if self.instance().resource_owned_by_own_instance(ty) {
            return Ok(rep);
        }
        self.resource_tables().guest_resource_lower_borrow(rep, ty)
    }

    /// Lifts a host-owned `own` resource at the `idx` specified into the
    /// representation of that resource.
    pub fn host_resource_lift_own(&mut self, idx: HostResourceIndex) -> Result<u32> {
        self.resource_tables().host_resource_lift_own(idx)
    }

    /// Lifts a host-owned `borrow` resource at the `idx` specified into the
    /// representation of that resource.
    pub fn host_resource_lift_borrow(&mut self, idx: HostResourceIndex) -> Result<u32> {
        self.resource_tables().host_resource_lift_borrow(idx)
    }

    /// Lowers a resource into the host-owned table, returning the index it was
    /// inserted at.
    ///
    /// Note that this is a special case for `Resource<T>`. Most of the time a
    /// host value shouldn't be lowered with a lowering context.
    pub fn host_resource_lower_own(
        &mut self,
        rep: u32,
        dtor: Option<NonNull<VMFuncRef>>,
        instance: Option<RuntimeInstance>,
    ) -> Result<HostResourceIndex> {
        self.resource_tables()
            .host_resource_lower_own(rep, dtor, instance)
    }

    /// Returns the underlying resource type for the `ty` table specified.
    pub fn resource_type(&self, ty: TypeResourceTableIndex) -> ResourceType {
        self.instance_type().resource_type(ty)
    }

    /// Returns the instance type information corresponding to the instance that
    /// this context is lowering into.
    pub fn instance_type(&self) -> InstanceType<'_> {
        InstanceType::new(self.instance())
    }

    fn resource_tables(&mut self) -> HostResourceTables<'_> {
        let (tables, data) = self
            .store
            .0
            .component_resource_tables_and_host_resource_data(Some(self.instance));
        HostResourceTables::from_parts(tables, data)
    }

    /// See [`HostResourceTables::validate_scope_exit`].
    #[inline]
    pub fn validate_scope_exit(&mut self) -> Result<()> {
        self.resource_tables().validate_scope_exit()
    }
}

/// Contextual information used when lifting a type from a component into the
/// host.
///
/// This structure is the analogue of `LowerContext` except used during lifting
/// operations (or loading from memory).
#[doc(hidden)]
pub struct LiftContext<'a> {
    store_id: StoreId,
    /// Like lowering, lifting always has options configured.
    options: OptionsIndex,

    /// Instance type information, like with lowering.
    pub types: &'a Arc<ComponentTypes>,

    memory: &'a [u8],

    instance: Pin<&'a mut ComponentInstance>,
    instance_handle: Instance,

    host_table: &'a mut HandleTable,
    host_resource_data: &'a mut HostResourceData,

    task_state: &'a mut ComponentTaskState,

    /// Remaining fuel for this hostcall/lift operation.
    ///
    /// This is decremented for strings/lists, for example, to cap the size of
    /// data the host allocates on behalf of the guest.
    hostcall_fuel: usize,
}

#[doc(hidden)]
impl<'a> LiftContext<'a> {
    /// Creates a new lifting context given the provided context.
    #[inline]
    pub fn new(
        store: &'a mut StoreOpaque,
        options: OptionsIndex,
        instance_handle: Instance,
    ) -> LiftContext<'a> {
        let store_id = store.id();
        let hostcall_fuel = store.hostcall_fuel();
        // From `&mut StoreOpaque` provided the goal here is to project out
        // three different disjoint fields owned by the store: memory,
        // `CallContexts`, and `HandleTable`. There's no native API for that
        // so it's hacked around a bit. This unsafe pointer cast could be fixed
        // with more methods in more places, but it doesn't seem worth doing it
        // at this time.
        let memory =
            instance_handle.options_memory(unsafe { &*(store as *const StoreOpaque) }, options);
        let (task_state, host_table, host_resource_data, instance) =
            store.lift_context_parts(instance_handle);
        let (component, instance) = instance.component_and_self();

        LiftContext {
            store_id,
            memory,
            options,
            types: component.types(),
            instance,
            instance_handle,
            task_state,
            host_table,
            host_resource_data,
            hostcall_fuel,
        }
    }

    /// Returns the canonical options that are being used during lifting.
    pub fn options(&self) -> &CanonicalOptions {
        &self.instance.component().env_component().options[self.options]
    }

    /// Returns the `OptionsIndex` being used during lifting.
    pub fn options_index(&self) -> OptionsIndex {
        self.options
    }

    /// Returns the entire contents of linear memory for this set of lifting
    /// options.
    ///
    /// # Panics
    ///
    /// This will panic if memory has not been configured for this lifting
    /// operation.
    pub fn memory(&self) -> &'a [u8] {
        self.memory
    }

    /// Returns an identifier for the store from which this `LiftContext` was
    /// created.
    pub fn store_id(&self) -> StoreId {
        self.store_id
    }

    /// Returns the component instance that is being lifted from.
    pub fn instance_mut(&mut self) -> Pin<&mut ComponentInstance> {
        self.instance.as_mut()
    }
    /// Returns the component instance that is being lifted from.
    pub fn instance_handle(&self) -> Instance {
        self.instance_handle
    }

    #[cfg(feature = "component-model-async")]
    pub(crate) fn concurrent_state_mut(&mut self) -> &mut ConcurrentState {
        self.task_state.concurrent_state_mut()
    }

    /// Lifts an `own` resource from the guest at the `idx` specified into its
    /// representation.
    ///
    /// Additionally returns a destructor/instance flags to go along with the
    /// representation so the host knows how to destroy this resource.
    pub fn guest_resource_lift_own(
        &mut self,
        ty: TypeResourceTableIndex,
        idx: u32,
    ) -> Result<(u32, Option<NonNull<VMFuncRef>>, Option<RuntimeInstance>)> {
        let idx = self.resource_tables().guest_resource_lift_own(idx, ty)?;
        let (dtor, instance) = self.instance.dtor_and_instance(ty);
        Ok((idx, dtor, instance))
    }

    /// Lifts a `borrow` resource from the guest at the `idx` specified.
    pub fn guest_resource_lift_borrow(
        &mut self,
        ty: TypeResourceTableIndex,
        idx: u32,
    ) -> Result<u32> {
        self.resource_tables().guest_resource_lift_borrow(idx, ty)
    }

    /// Lowers a resource into the host-owned table, returning the index it was
    /// inserted at.
    pub fn host_resource_lower_own(
        &mut self,
        rep: u32,
        dtor: Option<NonNull<VMFuncRef>>,
        instance: Option<RuntimeInstance>,
    ) -> Result<HostResourceIndex> {
        self.resource_tables()
            .host_resource_lower_own(rep, dtor, instance)
    }

    /// Lowers a resource into the host-owned table, returning the index it was
    /// inserted at.
    pub fn host_resource_lower_borrow(&mut self, rep: u32) -> Result<HostResourceIndex> {
        self.resource_tables().host_resource_lower_borrow(rep)
    }

    /// Returns the underlying type of the resource table specified by `ty`.
    pub fn resource_type(&self, ty: TypeResourceTableIndex) -> ResourceType {
        self.instance_type().resource_type(ty)
    }

    /// Returns instance type information for the component instance that is
    /// being lifted from.
    pub fn instance_type(&self) -> InstanceType<'_> {
        InstanceType::new(&self.instance)
    }

    fn resource_tables(&mut self) -> HostResourceTables<'_> {
        HostResourceTables::from_parts(
            ResourceTables {
                host_table: self.host_table,
                task_state: self.task_state,
                guest: Some(self.instance.as_mut().instance_states()),
            },
            self.host_resource_data,
        )
    }

    /// See [`HostResourceTables::validate_scope_exit`].
    #[inline]
    pub fn validate_scope_exit(&mut self) -> Result<()> {
        self.resource_tables().validate_scope_exit()
    }

    /// Consumes `amt` units of fuel, typically a number of bytes, from this
    /// context.
    ///
    /// Returns an error if the fuel is exhausted which will cause a trap in the
    /// guest. Note that this is distinct from Wasm's fuel, this is just for
    /// keeping track of data flowing from the guest to the host.
    pub fn consume_fuel(&mut self, amt: usize) -> Result<()> {
        match self.hostcall_fuel.checked_sub(amt) {
            Some(new) => self.hostcall_fuel = new,
            None => bail!(HostcallFuelExhausted),
        }
        Ok(())
    }

    /// Same as [`Self::consume_fuel`], but safely multiplies `len` and `size`
    /// together before calling that.
    pub fn consume_fuel_array(&mut self, len: usize, size: usize) -> Result<()> {
        match len.checked_mul(size) {
            Some(bytes) => self.consume_fuel(bytes),
            None => bail!(HostcallFuelExhausted),
        }
    }
}

#[derive(Debug)]
struct HostcallFuelExhausted;

impl fmt::Display for HostcallFuelExhausted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "too much data is being copied between the host and the guest: \
             fuel allocated for hostcalls has been exhausted"
        )
    }
}

impl core::error::Error for HostcallFuelExhausted {}
