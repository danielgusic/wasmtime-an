# AN-Encoding Changelog & additional information

This fork adds AN-encoding to wasmtime. Below you will find:
- The files that changed and what changed in them
- How AN-encoding works
- Design choices we made
- Tests
- Demo commands

The implementation is based on the paper Fetzer, Schiffel, Süßkraut, *AN-Encoding Compiler*, 2009.

---
## Enable

| Surface | How |
|---|---|
| `Config` (Rust API) | `config.an_encoding(true)` (and optionally `config.an_constant(a)`) |
| CLI | `wasmtime ... -C an-encoding=y [-C an-constant=N] ...` |
| `Tunables` fields | `an_encoding: bool`, `an_constant: u64` |

---
## What is AN-encoding?
> The AN-code is one of the most widely known arithmetic codes. Encoding
is done by multiplying the information part x_f of variable x with a constant A.
Thereby, the encoded version x_c is obtained. Only multiples of A are valid code
words and every operation processing AN-encoded data has to conserve this
property. Code checking is done by computing the modulus with A, which is zero for a valid code word.

Fetzer, Schiffel, Süßkraut, *AN-Encoding Compiler*, 2009.

In plain terms: every number is stored as itself multiplied by `A`. If some bit of memory gets corrupted, the stored value stops being a clean multiple of `A`, and we can catch that by checking `value mod A == 0`.

`A` defaults to `wasmtime_environ::DEFAULT_AN_CONSTANT` (`65521`, a 16-bit
prime the paper recommends). The setter checks that `1 ≤ A < 2²⁴` (A=0 could
never be decoded and would cause other problems [don't even try it!]; keeping A < 2²⁴ means the 8-bit LUT entries `A·(c1 OP c2) ≤ A·255` fit in 32 bits, so the tables are half the size).
Pick a large odd value (a prime is best) — powers of two make corruption harder to detect.

---

## Changes



### Runtime — `crates/wasmtime`

- **`runtime/an_lut.rs`** *(new)* — builds the per-`A` 256×256 `u32` lookup tables for AND/OR/XOR (`A·(c1 OP c2)`, 256 KiB each).
- **`engine.rs`** — `EngineInner` owns the `AnLuts`; `an_lut_addr(op)` gives out table addresses (works the same in any process, since it's JIT).
- **`runtime.rs`** — registers the `an_lut` module.
- **`runtime/vm/instance.rs`** — holds the per-instance AN state (LUT slots, `an_enc_shadows`, `an_whole_dirty` flags), the code that mirrors and encodes shadow memory, the atomic range-union re-encode and the capacity-keeping grow routines (including shadow access for imported memory and shadow growth on `memory_grow`), plus the range/whole-memory checks (`an_cross_check_range` / `an_cross_check_memory`) and the `an_is_whole_dirty` peek that lets reads skip memories that are known to be temporarily stale.
- **`runtime/vm/instance/allocator.rs`** — copies data-segment / CoW setup into each shadow before wasm code starts running.
- **`runtime/memory.rs`** — write hooks (`write` re-encodes the range it wrote; `data_mut`/`data_and_store_mut` mark the memory as whole-dirty) and read-side checking done right before use: `read` checks its exact range, `data`/`data_mut` check the whole memory when borrowed — the plain versions *panic* on a mismatch, while the fallible `try_data`/`try_data_mut`/`try_data_and_store_mut` versions return `Err(Trap::AnMemoryMismatch)` instead. `Memory::read`'s `MemoryAccessError` now names an AN mismatch instead of a generic "out of bounds" error. There are also `#[doc(hidden)]` shadow / check helpers used by the lifting, transcode-source, and wiggle code paths, plus one used only in tests.
- **`runtime/store.rs`** — `an_all_instance_ids()` (every instance, including hidden host-memory owners) drives a store-wide sweep for dirty memory; `an_heal_whole_dirty()` runs that sweep at the point where control passes from host back into wasm (`invoke_wasm_and_catch_traps`, in `runtime/func.rs`), so a `data_mut` done between top-level calls gets re-encoded before the guest's load check runs — the `an_resync_host_boundary` libcall calls the same sweep. Also `wasm_stack_raw_parts_for_test`, a `#[doc(hidden)]` helper that exposes the guest's native stack window `[stack_limit, last_wasm_entry_sp)` for outside stack fault-injection experiments (SWI); nothing in this repo calls it.
- **`runtime/vm/libcalls.rs`** — the `an_resync_host_boundary` libcall (run after a host call) sweeps and re-encodes any memory marked `data_mut`-dirty, using the store's `an_heal_whole_dirty`. `memory.copy` checks its source range before copying (so a bad re-encode of the destination can't hide corrupted source data) and `memory.copy`/`memory.fill`/`memory.init` re-encode the range they wrote after the fact (including imported destinations, via `an_encode_imported_range_from_raw`); if a boundary slot doesn't match during that re-encode, it traps with `AnMemoryMismatch`.
- **`runtime/component/func/{options,host}.rs`, `func.rs`, `func/typed.rs`, `values.rs`** — Lowering: `LowerContext` records the ranges the host writes (`get`/`slice_mut` record the exact range, `as_slice_mut` records the whole memory — `as_slice_mut` also checks the whole memory *before* the borrow and panics on a mismatch, just like `Memory::data_mut`; a separate `as_slice_mut_untracked` skips both checks for borrows that only validate bounds and never write) and `an_flush_dirty` re-encodes those ranges before wasm runs again (a mismatch becomes `Trap::AnMemoryMismatch`). Lifting: `LiftContext` keeps the options memory's shadow and `A`, and checks every lifted range (`memory_checked`, plus the lazy `WasmStr::to_str` / `WasmList::as_le_slice` accessors); every list/string/record/map/param lift site in `func/typed.rs`, `func/host.rs` and `values.rs` goes through it.
- **`runtime/component/concurrent/futures_and_streams.rs`** — the three `as_slice_mut()` borrows that only check bounds now use `as_slice_mut_untracked()` instead (they never write, so tracking them as a whole-memory write, or checking them up front, would be wrong). Nothing else here supports AN: component-model async is refused under AN by the feature mask.
- **`runtime/component/instance.rs`, `runtime/vm/component.rs`** — a per-`RuntimeMemoryIndex` map of AN identity and lookups used by the lowering flush and transcoder resync; `an_options_shadow` / `an_options_whole_consistent` (used by lifting and `as_slice_mut` checks) and `an_check_transcode_src` (checks transcode source); `an_core_memory_for_test` exposes a component's core memory for fault-injection tests.
- **`runtime/vm/component/libcalls.rs`** — each string transcoder checks its source range (`an_check_transcode_src`) before transcoding and re-encodes its destination range after.
- **`runtime/externals/global.rs`** — encoding/decoding of integer globals at the host boundary (`Global::get`/`set`), only for wasm i32/i64 globals. `Global::get` checks the codeword is valid (`enc % A == 0`) before decoding: the plain `get` *panics* on a bad codeword, the fallible `try_get` returns `Err(Trap::AnCodewordInvalid)`. There are test-only hooks that inject bad i32/i64 codewords.
- **`runtime/trampoline/global.rs`** — encodes the starting value of a host-created (`Global::new`) integer global.
- **`runtime/vm/vmcontext.rs`** — `VMGlobalDefinition::{from,to}_val_raw` encode/decode i32/i64 `ValRaw` to and from storage.
- **`compile.rs`** — `validate_an_encoding_constraints` (for core modules and component cores): rejects shared memory, floats, atomics, reference-type *values* (in signatures, globals, locals), and non-`funcref` tables; SIMD/GC/exceptions/stack-switching/component-async are rejected via the `config.rs` feature mask. `funcref` tables are still allowed (needed for `call_indirect`).
- **`config.rs`** — the `an_encoding`/`an_constant` setters, test fault-injection knobs, the AN feature mask (including `CM_ASYNC`/`CM_ASYNC_STACKFUL`), and rejecting Winch.
- **`engine/serialization.rs`** — AN tunables are now part of cwasm compatibility checks.

### Wiggle — `crates/wiggle`

- **`src/lib.rs`, `src/guest_type.rs`, `generate/src/wasmtime.rs`** — `GuestMemory` now can carry an optional write-range recorder and (under AN) a read-only shadow slice plus `A`: writes go through the integer `write` code and get their range recorded, and every host read (`GuestPtr::read`, `as_slice`/`as_cow`/`as_str`, `to_vec`) compares its byte range against the shadow with `an_cross_check_read`, returning `GuestError::AnMemoryMismatch` if they don't match (bytes the same call already wrote are skipped). The generated WASI p1 wrapper hands the view the shadow (`Memory::an_untracked_data_shadow_and_store_mut`) and re-encodes the recorded write ranges after the host body runs. `guest_error.rs` gets a new `AnMemoryMismatch` variant.
- **`test-helpers/src/lib.rs`** — now built with the `GuestMemory::unshared(...)` constructor.
- **`tests/an_dirty.rs`** *(new)* — unit tests for the write-range recorder.
- **`tests/an_read.rs`** *(new)* — unit tests for the read-side check done right before use.

### WASI — `crates/wasi`, `crates/wasi-common`

- **`wasi/src/p1.rs`, `wasi-common/src/snapshots/preview_1/error.rs`** — map `GuestError::AnMemoryMismatch` to a trap in `From<GuestError>` (in both the preview1 marshalling path and a user trait body), so an AN read mismatch is fatal rather than being turned into an errno.

### Environment — `crates/environ`

- **`vmoffsets.rs`** — three LUT pointer slots in `VMContext`, a per-memory `defined_memories_enc_bases` array, and `VMMemoryImport::an_enc_base_slot` (a pointer to the owner's enc-base slot; its address stays stable across `memory.grow`).
- **`builtin.rs`** — declares the `an_resync_host_boundary` builtin (`-> bool`; false means trap).
- **`trap_encoding.rs`** — the new `Trap::AnMemoryMismatch` (48), `AnCodewordInvalid` (49), and `AnI64WidenOverflow` (50).
- **`tunables.rs`** — `DEFAULT_AN_CONSTANT = 65521`, `ENC_MEM_GROWTH_FACTOR = 2`, and the AN tunable fields.
- **`module.rs`** — `Module::an_raw_globals` records imported host-control globals (`InstanceFlags` / `TaskMayBlock`) as values that cross the raw/encoded boundary.
- **`component/translate/adapt.rs`** — fills in `an_raw_globals` when an adapter module is translated.

### C API — `crates/c-api`

- **`src/trap.rs`**, **`include/wasmtime/trap.h`** — mirror the AN trap codes (`AnMemoryMismatch`, `AnCodewordInvalid`, `AnI64WidenOverflow`) for the C API.

### Cranelift — `crates/cranelift`

- **`lib.rs`** — `wasm_stack_value_type` widens i32 to I64 and i64 to I128 under AN; adds the `TRAP_AN_MEMORY_MISMATCH` / `TRAP_AN_CODEWORD_INVALID` / `TRAP_AN_I64_WIDEN_OVERFLOW` codes.
- **`translate/an_helpers.rs`** *(new)* — all the AN codegen helpers: encode/decode, bitwise-LUT, mul, shifts/rotates, shadow-store read-modify-write, the per-load validity check, and boundary codeword checks.
- **`translate/code_translator.rs`** — per-op AN encoding (see *Per-op behaviour*), keeping the shadow in sync for integer stores, decoding addresses/indexes around `memory.*`/`table.*`/`call_indirect` and `br_table`, and letting other globals pass through unchanged.
- **`func_environ.rs`** — widens i32 global storage to I64 and i64 global storage to I128 (and const-folds encoded immediate values); raw host-control globals stay native, encoded on get and decoded on set.
- **`translate/func_translator.rs`, `translate/translation_utils.rs`** — widen integer local/block-param IR types under AN (`i32`→`I64`, `i64`→`I128`).
- **`translate/mod.rs`** — re-exports the AN helper entry points used outside the translator (`emit_an_codeword_validity_check`, i64 encode/decode helpers, and `iconst_i128`).
- **`compiler.rs`** — the wasm/host trampolines encode/decode integer scalars and check boundary codewords, and emit the `an_resync_host_boundary` libcall after the call to heal dirty memory.
- **`compiler/component.rs`** — the same treatment for `translate_hostcall`; plus i32 decode/encode in the transcode, `resource_drop`, and `UnsafeIntrinsic` (load/store/context) trampolines.

### CLI & tests

- **`crates/cli-flags/src/lib.rs`** — `-C an-encoding=y`, `-C an-constant=N`.
- **`an_encoding/`** *(new)* — wat modules used by the tests (`conversions.wat`, `fib.wat`, `int_conversions.wat`, `mul.wat`, `ops.wat`), and the `fib.rs` demo source.
- **`tests/all/an_encoding.rs`** *(new)* — the AN test suite (see *Tests*).
- **`tests/all/main.rs`** — registers `mod an_encoding;`.

---

## Design

How values look inside an AN-encoded module:

| Wasm type | IR type | Holds |
|---|---|---|
| `i32` | `I64` | `A·v` with canonical `v ∈ [0, 2³²)` |
| `i64` | `I128` | `A·v` with canonical `v ∈ [0, 2⁶⁴)`, so `A·v < 2⁸⁸` |

See *Supported and unsupported features* below for more.

**Memory model:** two linear memories, one mirroring the other but encoded.

Everything you do with linear memory still happens as before, but it's also mirrored and encoded into a shadow memory when AN-encoding is on. So there end up being two memories, roughly tripling memory use.

**Shadow checking/resync:** We only check correctness for the relevant ranges, right before reading, one slot at a time (`[slot] == A * u32_le(raw[4i..4i+4])`, one multiply plus one compare).

**Note:** For writes that aren't slot-aligned, we also check the untouched bytes that got pulled in by rounding to slot size, so corruption can't be hidden that way.

| Read path | Check |
|---|---|
| guest `i32.load{,8,16}` | an inline per-slot check right at the load. A mismatch causes `Trap::AnMemoryMismatch`. |
| host `Memory::read` | checks exactly `[offset, offset+len)` before copying (`an_range_consistent`). On a mismatch it returns `Err(MemoryAccessError)`; this typed return can't carry the `AnMemoryMismatch` trap *code*, but its message is updated to name the AN mismatch rather than say "out of bounds". |
| host `Memory::data` / `data_mut` | an opaque whole-memory borrow, so the whole memory is checked when borrowed. The plain versions *panic* on a mismatch. The fallible twins `try_data`/`try_data_mut`/`try_data_and_store_mut` return `Err(Trap::AnMemoryMismatch)` instead (`try_data_mut` checks before marking the memory whole-dirty). |
| host `WasmList::as_le_slice` | checks the list's bytes when borrowed. The plain version *panics*; the fallible twin `try_as_le_slice` returns `Err(Trap::AnMemoryMismatch)`. |
| host `Global::get` (i32/i64) | checks the codeword is valid (`enc % A == 0`) before decoding. The plain version *panics* on a bad codeword; the fallible twin `try_get` returns `Err(Trap::AnCodewordInvalid)`. |
| component lifting (`LiftContext`) | a per-range check on every lifted value (`memory_checked` for the `cx` paths; `an_options_shadow` for the lazy `WasmStr::to_str` / `WasmList::as_le_slice`). A mismatch causes `Trap::AnMemoryMismatch`. |
| component transcoders (fused string adapters) | the source range is checked before transcoding (`an_check_transcode_src`); the destination is re-encoded after. |
| `memory.copy` | the source range is checked before the copy (so a bad re-encode of the destination can't hide corrupted source data). |
| WASI preview1 (wiggle reads) | an exact-range check in the `GuestMemory` view (`GuestPtr::read`, `as_slice`/`as_cow`/`as_str`, `to_vec`) against a read-only shadow handed out when the hostcall starts; bytes the same call already wrote are skipped. This surfaces as `AnMemoryMismatch`, which `wasmtime-wasi` / `wasi-common` turn into a **trap**. |
| host `Memory::data_ptr`, raw escape hatches | **not checked** — this is a raw pointer with no range or lifetime tracking, by design. |

Host writes are tracked per code path, and only the parts the host actually wrote get re-encoded in the shadow.

Write paths:

| Host write path | Shadow maintenance |
|---|---|
| `Memory::write` | re-encodes the exact range immediately, right at the write (this also works *outside* host calls — see the semantics changes below) |
| `Memory::data_mut` / `data_and_store_mut` | sets a whole-dirty flag on the memory, which is cleared by a full re-encode at the next heal — this happens at host-call entry/exit *and* at the host→wasm **entry** sweep (`an_heal_whole_dirty`), so a `data_mut` write done between top-level calls gets re-encoded before the guest's load check runs (otherwise it would wrongly trap on the stale shadow) |
| WASI preview1 (wiggle) | `GuestMemory` records every byte range that's written (typed `write`, `copy_from_slice`, `as_slice_mut`); the generated hostcall wrapper collects the ranges after the host body returns, checks their combined range, then re-encodes exactly those bytes in one atomic step |
| component canonical ABI (incl. WASI preview2) | `LowerContext` records ranges (`get` / `slice_mut` record the exact range; the raw `as_slice_mut` falls back to the whole memory) and re-encodes their combined, checked range right away before wasm runs again: at `realloc` entry, after host→wasm argument lowering (`with_lower_context`), and after host-result lowering (`lower_result_and_exit_call`) |
| component transcode libcalls (fused adapters) | each transcoder re-encodes its destination range; the raw `dst` pointer is matched back to its owning memory using the per-`RuntimeMemoryIndex` identity map captured at `extract_memory` |
| raw `Memory::data_ptr` writes | **not tracked** — indistinguishable from corruption. This can be used on purpose for fault injection, though. |

Some design decisions about memory:
- The extra shadow memory is 2x the size of the regular memory
- Correctness is only checked for the relevant segments (with the exceptions noted above): per-slot on every guest load and host read; there's never a periodic sweep of the whole memory
- Wasm-side stores keep the shadow in step through the JIT-generated mirror code; host-side writes re-encode just the touched ranges, as described above
- Unaligned and subword accesses use read-modify-write, as in the paper
- Shared/atomic memories are rejected when AN-encoding is on; multiple memories are supported. Imported (non-shared) memories are supported too: the importing instance's JIT code reaches the owner's shadow through the stable `VMMemoryImport::an_enc_base_slot` pointer (one extra load), so if the owner does a `memory.grow` — which can move the shadow in memory — importers don't notice

**Function signatures:** Internal wasm function signatures are widened: wasm `i32`
params/results become IR `I64`, and wasm `i64` params/results become IR
`I128`. Trampolines convert values at the wasm/host boundary so anything
outside (host functions, the embedder) still only ever sees plain integer values.

**AN-encoding injection:** We change operations while wasm is being translated to CLIF (Cranelift's intermediate representation), because we need information from wasm at that point that CLIF doesn't have (like which stores touch linear memory and so need to update the shadow).
It's also the earliest point in the whole compilation pipeline.
> The earlier the encoding is done, the larger is the sphere of protection.

Fetzer, Schiffel, Süßkraut, AN-Encoding Compiler, 2009.

**Canonical invariant:** After every operation, encoded integer values are
brought back into their canonical range (`i32`: `A·v` with `v ∈ [0, 2³²)`;
`i64`: `A·v` with `v ∈ [0, 2⁶⁴)`). This is what lets compares, addresses, and
host-call arguments work directly on the encoded form when the operation allows it.

### Supported and unsupported features

| Feature | Status under AN-encoding |
|---|---|
| `i32` | everything based on i32 should work, except conversions to unsupported types (floats) and atomics (see *Per-op behaviour*) |
| linear memory (incl. multi-memory, bulk-memory, `memory.grow`) | uses an encoded shadow, checked right before use (per-slot on guest loads + host reads) |
| tables, `call_indirect` | i32 index/length operands are decoded around the builtin |
| `i64` | encoded as `I128` (`A·v`), see the i64 notes under *Per-op behaviour* |
| imported (non-shared) memories | stores mirror through the owner's shadow via `VMMemoryImport::an_enc_base_slot` |
|floats|**rejected** at compile time
|SIMD|**rejected** at compile time
| shared (atomic) memories, atomic operators | **rejected** at compile time |
| reference types as *values* (externref, funcref-as-value, …) in signatures / globals / locals | **rejected** at compile time (these are opaque host handles, with no integer to encode) |
| non-`funcref` table element types (externref / GC / exn / cont tables) | **rejected** at compile time |
| component-model async (futures / streams) | **rejected** via the feature mask under AN |
| GC / exceptions / stack switching proposals | **rejected** via the feature mask under AN (the mask also covers relaxed-SIMD, legacy exceptions, and shared-everything-threads) |
| Winch | **rejected** at config validation |
| wmemcheck | nothing was done for it specifically, it probably breaks|

### Per-op behaviour

The table below describes how i32 works. Encoded i64 values mostly use
the same approach on the wider canonical band (`A·v`, `v ∈ [0, 2⁶⁴)`) with IR
type `I128`; the i64-specific differences follow the table.

| Op | Strategy |
|---|---|
| `i32.const k` | Emit `iconst.i64 (A·k)` |
| `i32.add` | `iadd`, then bring it back into range by checking for overflow: `sum >= A·2³² ? sum - A·2³² : sum` |
| `i32.sub` | `isub`, then bring it back into range by checking for underflow: `diff < 0 ? diff + A·2³² : diff` |
| `i32.mul` | `(P_hi, P_lo) = (umulhi, imul)(A·n, A·m) → udiv_u128_by_u64_const(·, A) → umod_u128_by_u64_const_to_i64(·, A·2³²)`. See the *i32.mul* note below |
| `i32.div_u` | `(arg1 udiv arg2) · A` (one factor of A cancels out naturally) |
| `i32.rem_u` | Unchanged: `A·n urem A·m = A·(n urem m)`  |
| `i32.div_s` | The sign is found via `enc ≥ A·2³¹`, the encoded absolute value via `aw − enc` (with `aw = A·2³²`), then `udiv` runs on the absolute values (A cancels out), the result is re-encoded (`· A`), and the sign is re-applied (`s1 ⊕ s2`). There's an explicit `INT_MIN/-1 -> INTEGER_OVERFLOW` trap on the encoded operands before the absolute-value step, and a `/0` trap via `translate_udiv` on `abs2` (since `abs2 = 0` exactly when `arg2 = 0`). A zero quotient's sign is special-cased so it stays in canonical form. |
| `i32.rem_s` | Same sign-detection and absolute-value trick, but uses `urem`, which keeps the `A` factor (`urem(A·\|n\|, A·\|m\|) = A·(\|n\| urem \|m\|)`), so no re-encode is needed. The result takes the dividend's sign. `INT_MIN%-1` naturally works out to `urem(A·2³¹, A) = 0` (no trap, matching wasm). |
| `i32.eqz` | Checks the operand's codeword, does `icmp_imm Equal arg 0` to get an i8 boolean, then `select(bool, A, 0)` to encode it as `0`/`A`. The check is needed because the boolean is a *fresh* codeword: without it, corruption in the operand could slip through the compare undetected |
| `i32.lt_u`, `le_u`, `gt_u`, `ge_u`, `eq`, `ne` | Checks both operands' codewords (same reasoning as `eqz`, since the result is a fresh codeword), compares the encoded operands directly (A preserves order and zero), then `select(bool, A, 0)` to encode the boolean result |
| `i32.lt_s`, `le_s`, `gt_s`, `ge_s` | Checks both operands' codewords, remaps each to `c' = (c + A·2³¹) mod (A·2³²)`, then does an unsigned compare |
| `i32.and`, `i32.or`, `i32.xor` | Uses lookup tables over functional 8-bit chunks, via `emit_an_bitwise_i32` (as in the paper). One `udiv` per operand decodes it; four `(c1<<8)\|c2` indexes look up `A·(c1 OP c2)` in a 256×256 `u32` table (zero-extended to `i64`), then `acc += entry << (8·i)` recombines them into `A·(n OP m)`. The tables live on the `Engine` (one per `A`, built by `crates/wasmtime/src/runtime/an_lut.rs`); their address is loaded from a fixed `VMContext` slot right at the op (`load.i64 [vmctx + offset]`), so the same machine code works no matter what process it runs in. |
| `i32.shl` | Decodes the count (`udiv enc_k, A`), masks it `& 31`. The value stays encoded: the helper `emit_an_shl_i32` computes `enc_v · 2^k`, then brings it back into range using the existing 128/64 `umod_u128_by_u64_const_to_i64` against `A·2³²`. |
| `i32.shr_u` | Decodes the count, masks it `& 31`. `udiv(enc_v, A·2^k)` naturally cancels `A` out of the dividend, giving the raw `v >> k`; this is re-encoded with `· A`. **Note:** the paper decodes the count and uses it as a lookup-table index instead. |
| `i32.shr_s` | Reuses `emit_an_shr_u_i32` for the logical part, then `iadd`s an encoded sign-extension mask if `enc_v ≥ A·2³¹` (negative). The mask is `A·((1<<k)−1)·2^(32−k) = aw − (aw >> k_mod)` — two instructions, unlike the paper's `signExt[]` table. The addition is exact because the logical shift's result already has its top `k` bits clear. **Note:** same idea as above. |
| `i32.rotl`, `i32.rotr` | `(v << k_mod) \| (v >> (32−k_mod))`. The bit ranges don't overlap, so OR is the same as ADD on the encoded sums. Implemented as `iadd(emit_an_shl_i32, emit_an_shr_u_i32)` with the right shift amounts. Both helpers handle shifts in `[0, 32]`; at `k_mod = 0` the "other side" shift naturally comes out to 0 (`shl(_, 32)` is 0 mod `aw`; `shr_u(_, 32)` is 0 since `enc_v < aw`), so a no-op rotation just falls out without needing a special case. |
| `i32.clz`, `i32.ctz`, `i32.popcnt` | Decodes once (`udiv enc, A`), `ireduce.i32`, runs the native op, `uextend.i64`, then re-encodes with `· A`. **Note:** this can't be done without decoding first, since it's inspecting individual bits (as far as we know). |
| `i32.load{,8_u,16_u,8_s,16_s}` | Decodes the address (÷A → trunc.i32), does the wasm load (raw), `uextend.i64`, then `·A`. Loads read from raw memory. There's an inline check (`enc_slot == A * u32_le(raw_slot)`) right after the raw load, for every shadow slot it touched (after the raw load's own bounds check — so a guard-page-protected out-of-bounds access traps first and the shadow buffer is never indexed out of bounds — but before the loaded value is used); on a mismatch it traps `AnMemoryMismatch` right away. |
| `i32.store` (4-byte) | Decodes the address and the value (÷A → trunc.i32); does the raw wasm store. **Plus** it keeps the AN-encoded mirror updated: it branches at runtime on `effective_addr & 3`. The aligned path (`byte_pos == 0`) does one `store.i64 [enc_base + 2*effective_addr]` of the encoded value `A*v`. The unaligned path breaks it into four byte read-modify-writes at consecutive byte addresses; each helper works out its own slot index, so crossing a slot boundary is handled automatically. |
| `i32.store8` | Decodes the address and the value; does the raw byte store. **Plus** one byte read-modify-write on the shadow slot holding the target byte. `i32.store8` always fits in a single slot. |
| `i32.store16` | Decodes the address and the value; does the raw half-word store. **Plus** two byte read-modify-writes at `effective_addr` and `effective_addr + 1`. This handles both the in-slot case (`byte_pos in 0..=2`) and the cross-slot case (`byte_pos == 3`) the same way, since each byte read-modify-write works out its own slot index. |
| `local.{get,set,tee}` (i32) | The type is widened to `I64` by the signature/locals widening. |
| `global.get` (i32) | `I32` globals are stored already encoded, so nothing extra is needed for the guest at each access. Storage is widened to `I64`; imports, defined globals, and constant-folded immutable globals all load the encoded form directly. Decoding only happens at external boundaries. |
| `global.set` (i32) | The operand is already the canonical encoded `A·v`, so nothing changes on the guest path. Non-integer globals pass through unchanged. Encoding/decoding only happens at external boundaries. |
| `i32.extend8_s` / `i32.extend16_s` | Keeps the low encoded byte/half-word with `enc mod (A·2^bits)`, then adds the encoded sign-extension correction if the low sign bit is set. The value stays encoded and keeps the same residue modulo `A`. |
| `br_if` / `if` / `select` cond | Checks the condition's codeword, then branches/selects on the encoded value directly (`A·v ≠ 0` exactly when `v ≠ 0`). The check is needed because the condition is *consumed* without producing a codeword of its own: without it, corruption could slip through the control-flow decision. (`select` on encoded i64 values also splits the `I128` select into two i64 selects — otherwise cranelift's egraph folds it into an `umin.i128` it can't lower.) |
| host-import call (wasm → host) | Decodes encoded integer args, encodes integer results, at the `wasm_to_array` trampoline. After the call the trampoline emits the `an_resync_host_boundary` libcall — this **only heals dirty memory** (re-encodes memories the host borrowed wholesale via `Memory::data_mut`; clears the whole-dirty flag). The old whole-memory check that used to run before the call is gone — corruption is now caught at use (guest load / host read) — and the old pre-call libcall is gone too (it would be a no-op: nothing is whole-dirty going into a host call). Range-tracked host writes (`Memory::write`, wiggle, component lowering) re-encode their exact ranges right at the write. A **boundary codeword check** runs on every encoded integer arg before the `udiv` decode: `val % A != 0 → Trap::AnCodewordInvalid`. |
| host → wasm entry call | Encodes integer args, decodes encoded integer results, at the `array_to_wasm` trampoline. A **boundary codeword check** runs on every encoded integer result before the `udiv` decode. |

For i64, these operators follow the same AN approach as their i32 counterparts,
just with `I128`, 64-bit raw values, and the canonical band `A·2⁶⁴`: `const`,
`add`, `sub`, `eqz`, all integer compares, `extend8_s` / `extend16_s` /
`extend32_s`, `clz`, `ctz`, `popcnt`, `shl`, `shr_u`, `shr_s`, `rotl`, `rotr`,
`and`, `or`, `xor`, `local.{get,set,tee}`, guest-side `global.get/set`,
memory64 address decode, and `load/store{,8,16,32}`. Signed comparisons use
the same bias-remap idea at `A·2⁶³`; i64 boolean results are still encoded as i32
booleans (`0` / `A`). Full i64 memory accesses still follow the same shadow rule as i32:
raw memory stores bytes, and the shadow mirrors each 4-byte raw slot
as `A·u32_le(slot)`, so an 8-byte i64 access checks or updates the i32-sized shadow slots it touches.

The i64 cases that differ from the i32 baseline are:

| Op(s) | Difference |
|---|---|
| `i64.mul` | Stays encoded, but building `A²·n·m` can go over 128 bits. Since no 256-bit intermediate value is built, this overflow traps as `Trap::AnI64WidenOverflow`; otherwise, we divide by `A` to get the encoded product `A·n·m`, then bring it back into range modulo `A·2⁶⁴`. For a 128-bit `(q_hi, q_lo)` value, that's cheap: result = `(q_hi % A, q_lo)`, so the value never has to leave the encoding. |
| `i64.div_u/s`, `i64.rem_u/s` | Uses the software `emit_udivrem_i128` helper, because Cranelift has no general `udiv.i128` lowering. The AN math mirrors i32 (`A` cancels out for division, unsigned remainder keeps the factor), but the implementation is a software 128-bit long-division path. |
| `i32.wrap_i64` | Reduces an encoded i64 modulo `A·2³²`, giving an encoded i32. Per the wasm spec, this never traps. |
| `i64.extend_i32_s/u` | Widens directly in the encoded domain; the signed form adds `A·(2⁶⁴−2³²)` when the original i32's sign bit was set. |

### `i32.mul` note

To keep `i32.mul` encoded, the division uses algorithm 4 from the paper "Improved Division by Invariant Integers", Möller & Granlund, 2010.
Here's the high-level idea (see `crates/cranelift/src/translate/an_helpers.rs` for the details):
1. Compute the raw product P  = (A·n) · (A·m) = A²·n·m
2. Compute the quotient Q  = P / A = A·n·m
3. Bring it back into range: R = Q mod (A·2³²) = A·(n·m mod 2³²)

Several helper functions were written to make this work.

### Validity checks

Codeword validity (`val % A == 0`) is checked at the wasm/host trampoline
boundaries (in both directions — core-wasm trampolines in `compiler.rs` and the
component-model `translate_hostcall` path in `compiler/component.rs`). It's
also checked on operands that get decoded into raw values (for example `clz`,
`and`, shift counts, and subword/unaligned `i32.store`), and on operands that are *consumed*
without producing a fresh codeword — compares/`eqz` (whose `{0, A}` boolean
result is fresh) and the `br_if`/`if`/`select` conditions (control-flow
decisions) — so corruption can't slip through those ops unnoticed. See *New traps* below.

Errors that happen during the decoding step itself are not detected.

For memory validity checks (the per-read-path *Read path/Check* table above) and
resync details, see *Shadow verification/resync* above.


### New traps

`Trap::AnMemoryMismatch` (variant `48`) fires when an encoded
shadow slot doesn't match the raw bytes:
- inline, right at the guest load site, when a `load` instruction reads memory — it fires at the exact load where the mismatch was seen.
- See *Shadow verification/resync* above for more on how memory is checked.

**Note:** There are now new `try_*` versions of some previously-panicking accessors (`Memory::data`/`data_mut`,
`WasmList::as_le_slice`, `LowerContext::as_slice_mut`, `Global::get`); the original versions now panic if they detect a mismatch.

`Trap::AnCodewordInvalid` (variant `49`) fires from the boundary codeword
validity check, at every wasm/host trampoline decode site. Specifically:

- `compile_wasm_to_array_trampoline` checks every encoded integer
  scalar argument before decoding it (when wasm calls into a host import).
- `array_to_wasm_trampoline` checks every encoded integer scalar
  result before decoding it (when host calls into wasm via the entry trampoline).
- the component hostcall trampoline (`translate_hostcall`) checks
  every encoded integer scalar parameter before decoding it.
- every op-internal decode site (see *Validity checks* above: `clz`, `and`,
  subword/unaligned `i32.store`, ...) checks the encoded operand
  before the decoding `udiv`.
- the host boundary `Global::get` (for i32 and i64) checks the encoded slot before
  decoding; `get` *panics* on an invalid codeword, `try_get` returns
  `Err(Trap::AnCodewordInvalid)`.

`Trap::AnI64WidenOverflow` (variant `50`) fires from `i64.mul` when the
still-encoded product `A²·n·m` doesn't fit in 128 bits (no 256-bit intermediate
value is built). This is the one place where AN-on and AN-off behave differently
for an operation that isn't outright refused. It should be rare, though, since most
`i64` values are probably pointers.

---

## Tests

The tests were generated with the help of AI.

```
cargo test -p wasmtime-cli --test all an_encoding::
```

grouped, with AN off and on:

| Test | Coverage |
|---|---|
| `mul_{without_an,with_an}` | `i32.mul` end-to-end on the native backend |
| `fib_{without_an,with_an}` | `an_encoding/fib.wat` end-to-end via WASI preview1 (`MemoryInputPipe` / `MemoryOutputPipe`) |
| `fib_with_an_and_load_validity_check` | the same fib run, exercising the WASI `fd_read` → post-host resync → wasm load chain, with the per-load check |
| `data_mut_between_calls_resynced_before_guest_load` | a legitimate `Memory::data_mut` write between top-level calls must be re-encoded at wasm entry before the guest's load check runs, otherwise it would falsely trap on a stale shadow (a regression guard for the entry heal) |
| `ops_{without_an,with_an}` | one wat module exporting one function per touched operator: add, sub, mul, divu, remu, divs, rems, addconst, lt_u, ge_u, gt_u, eq, ne, eqz, lt_s/le_s/gt_s/ge_s, and/or/xor/not/mask_merge, shl/shr_u/shr_s/rotl/rotr, clz/ctz/popcnt, max_u, loop_count, digits, memory load/store, mutable i32 global (g_get/g_set/g_inc), plus a negative immutable initializer. Shifts/rotations cover 12 value patterns × 14 shift counts (including wraparound past 32). Includes trap checks for `div_s` (`/0`, `INT_MIN/-1`) and `rem_s` (`/0`, `INT_MIN%-1 → 0`). |
| `ops_with_an_custom_constants` | re-runs the `ops_*` checks with several non-default values of `A` (1, 7, 1009, 2²⁴ − 1), to check that the codegen reads `A` from `Tunables` instead of baking in the default |
| `i64_addwrap_{without,with}_an` / `add64_{without,with}_an` / `i64ops_{without,with}_an` | core i64 ops, using AN-off as the reference. `add64`/`i64_addwrap` cover `i64.add`/`i64.sub` with wraparound at the `A·2⁶⁴` band edge; `i64ops` covers the full compare set (`lt`/`le`/`gt`/`ge` signed and unsigned, `eq`/`ne`/`eqz`) over a MIN/MAX/-1 boundary matrix checked against a Rust reference implementation (signed vs unsigned must disagree on the high half — the `A·2⁶³` bias remap), plus `extend8_s`/`extend16_s`/`extend32_s`. |
| `divrem_{without,with}_an` | `i64.div_u/div_s/rem_u/rem_s` over a matrix of sign combinations plus MIN/MAX/-1 dividends (against a Rust reference implementation), using the software 128-bit long-division helper (`emit_udivrem_i128`); checks the exact trap **code** — `IntegerDivisionByZero` for `/0` (every op, six dividends) and `IntegerOverflow` for `INT_MIN/-1` (div_s) — and that `INT_MIN % -1 = 0`. |
| `i64_bitwise_{without,with}_an` / `i64_shift_{without,with}_an` | `i64.and/or/xor` via the 8-chunk lookup table plus 128-bit accumulator, over chunk-crossing operand pairs, and `clz/ctz/popcnt` — all checked against a Rust reference implementation. `i64.shl/shr_u/shr_s/rotl/rotr` over an 8-value × 13-count matrix (counts including `≥ 64` to exercise the `&63` mask on every op, and negative values for `shr_s`/`rotr`), checked against the Rust reference implementation. |
| `mul64_{without,with}_an` / `mul64_overflow_traps_under_an` / `mul64_no_overflow_with_an_constant_1` | still-encoded `i64.mul` matches AN-off for products that stay within the 128-bit band; `mul64_overflow_traps_under_an` checks that the overflowing product `(1<<62)²` raises `Trap::AnI64WidenOverflow` for every legal `A > 4`; `mul64_no_overflow_with_an_constant_1` checks that `A=1` (identity encoding) never overflows the 128-bit product. |
| `i64_mem_{without,with}_an` / `i64_load_validity_check_traps_on_raw_tamper` | `i64.load`/`i64.store` (the two-independent-i32-shadow-slot decomposition) over aligned/unaligned/cross-slot offsets (the unaligned 8-byte span straddles three slots) × a MIN/MAX/all-ones/zero value set, plus the narrow `i64.store{8,16,32}` round-tripped through `i64.load{8,16,32}_{u,s}` at aligned/unaligned/cross-slot offsets. `i64_load_validity_check_traps_on_raw_tamper`: an untracked `data_ptr` tamper in either 4-byte half of a stored i64 makes the load trap with `AnMemoryMismatch`. |
| `i64_global_{without,with}_an` | a guest-side mutable i64 global (`get`/`set`/`inc`) stored in the widened `I128` slot, plus an immutable const-folded i64, using AN-off as the reference. |
| `i64_ops_various_an_constants` | the i64 version of `ops_with_an_custom_constants`: re-runs the whole guest-side i64 surface (add/sub/wrap, the op battery, div/rem, bitwise, shift/rotate, mul, memory, globals) across `A ∈ {1, 7, 1009, 2²⁴ − 1}`, proving the i64 codegen reads `A` from `Tunables` — in particular the software 128-bit long-division helper and the bitwise-lookup-table scaling. |
| `global_i64_boundary_{without,with}_an` / `global_i64_import_{without,with}_an` / `global_i64_codeword_setup` driving `global_i64_try_get_{clean_passes,invalid_codeword_traps}` / `global_i64_get_panics_on_invalid_codeword` | host-boundary i64 globals: `boundary` checks the host `Global::get`/`set` view against the guest view over a value matrix (including min/max/negatives); `import` exercises the `Global::new` host-storage path; the codeword trio checks that a non-multiple-of-`A` i64 slot makes `try_get` return `Err(Trap::AnCodewordInvalid)` and makes `get` panic. `boundary`/`import` re-run across `A ∈ {1, 7, 1009, 2²⁴ − 1}` via `global_boundary_various_an_constants`. |
| `codeword_check::codeword_check_clean_wasm_to_host_i64_params` / `codeword_check_clean_host_to_wasm_i64_returns` / `codeword_check_traps_wasm_to_host_i64_args_with_injection` / `codeword_check_traps_host_to_wasm_i64_returns_with_injection` | the i64 boundary codeword check, in both directions: clean i64 args/results pass; with `an_inject_codeword_fault`, the trampoline bumps the first encoded i64 arg/result so the modulo check traps with `Trap::AnCodewordInvalid`. |
| `component_codeword::component_i64_arg_passthrough_{without,with}_an` / `component_i64_various_an_constants` / `component_i64_codeword_check_traps_with_injection` | component-model i64 *scalar* params at the canonical-ABI hostcall trampoline: round-tripped across the full i64 range (including MIN/MAX) under AN, swept over `A ∈ {1, 7, 1009, 65521, 2²⁴ − 1}`; the fault-inject case checks that the component boundary codeword check fires just like the core path. |
| `global_boundary_{without,with}_an` / `global_import_{without,with}_an` / `global_boundary_various_an_constants` | host-boundary global encode/decode. `global_boundary_*` exports mutable and immutable i32/i64 globals directly and checks the host view (`Global::get`/`set`) against the guest view (`global.get`/`set`) over a value matrix (including negatives and min/max); the host always sees raw values while storage stays encoded. `global_import_*` imports host-created (`Global::new`) integer globals into the module, exercising the `VMGlobalKind::Host` storage path (host init + `set`/`get` + guest mutation round-trip). `_various` re-runs both under `A ∈ {1, 7, 1009, 2²⁴ − 1}`. The AN-off counterparts check that behavior is identical. |
| `refuse_float_{param,result,local,global,op}_under_an` | a float in a function signature, global, local, or operator stream must fail to compile under AN with a "floating-point" message |
| `refuse_shared_memory_under_an` | compiles a shared-memory wat module under AN and checks the error message mentions AN-encoding |
| `imported_memory_compiles_under_an` / `imported_memory_stores_mirror_owner_shadow` / `imported_memory_tamper_{raw,shadow}_traps` / `imported_memory_bulk_ops_keep_shadow` / `imported_memory_grow_through_importer` / `imported_memory_various_an_constants` / `host_created_memory_imported_under_an` | the imported-memory support matrix: an exporting instance owns the memory, and the importer stores/loads/fills/copies/grows through it; checking at use covers the import (clean runs pass, verified by guest-load read-backs; raw/shadow tampering is caught by a host `Memory::read` of the owner); a host-created `Memory::new` import works too, including the `Memory::write`/`data_mut` host-write paths; re-run across `A ∈ {1, 7, 1009, 65521, 2²⁴−1}` |
| `multi_memory_compiles_under_an` / `multi_memory_stores_keep_shadows_consistent` / `multi_memory_tamper_{mem0,mem1}_traps` | a multi-memory module with two defined memories: stores route to each one via `memarg.memory` (checked by guest-load read-backs), and tampering either memory's raw bytes is caught independently by a host `Memory::read` of that memory |
| `load_validity_check_clean_run_passes` / `load_validity_check_traps_on_{raw_tamper,load8u,load16u_cross_slot}` / `load_validity_check_traps_unaligned_i32_load` / `load_validity_check_various_an_constants` | the per-load check: tampering raw bytes via the untracked `Memory::data_ptr` path (NOT `data_mut`, which would legitimately resync) between instantiation and a wasm load makes the load raise `AnMemoryMismatch` right away. Covers `i32.load`/`load8_u`/`load16_u`, aligned + unaligned + cross-slot positions, and several `A` values. |
| `br_table_{without,with}_an` | a `br_table` with three explicit targets plus a default; checks that selectors 1/2 pick their arm and out-of-range selectors (3, 7) hit the default, under both AN-on and AN-off (selector 0 is left out: `A*0 == 0` makes a missing decode invisible there). This is a regression guard: the controlling index is a raw i32 selector and must be decoded before `br_table`, otherwise the encoded value (`A*v`) lands out of range and every non-zero index silently falls through to the default. This is the one index-consuming operator the rest of the matrix didn't cover. |
| `table_{size,grow,fill,copy,init}_under_an` / `call_indirect_under_an` / `table_ops_match_without_an` | a wat with a 4-element funcref table exercises each table op under AN-on and checks it matches the AN-off baseline. Without the per-operand decode, encoded i64 operands flowing into `cast_index_to_i64` panic in cranelift. `call_indirect` covers the vtable dispatch case (the hot path for closures / virtual calls in real wasm). |
| `component_an::component_compiles_{without,with}_an` / `component_an::component_with_an_various_constants` | component-model integration: a component wraps a core module that does an `i32.store` and then calls a host import via canon-lower. The AN dirty-heal and resync libcalls fire from the component hostcall trampoline using the core caller's vmctx. The "various constants" case re-runs across `A ∈ {1, 7, 1009, 65521, 2^24 − 1}`, checking that the libcalls read `A` from the engine tunables. |
| `component_an::transcode_component_compiles_{without,with}_an` | compiles a component that transcodes a string between encodings (utf8 → utf16) under AN (constants 1, 7, 65521, 2²⁴−1). A regression guard for the string-transcoder trampoline: before the fix, `uextend.i64` was applied to an already-encoded i64 ptr/len argument, which crashed cranelift's aarch64 lowering with `assert!(inner_bits < out_bits)`. |
| `component_an::transcode_string_roundtrip_{without,with}_an` | end-to-end: lowers a host `&str` into a component and reads back its UTF-8 byte length (ASCII `"hello"` → 5; multi-byte `"héllo"` → 6). Exercises the whole string-ABI path under AN: transcoder trampoline argument decode/result encode, the realloc call into AN-compiled core wasm, and the raw `may_enter`/`may_leave` instance-flag globals (encode-on-get / decode-on-set). Before the flag fix, this trapped with "cannot leave component instance". |
| `component_an::resource_new_drop_{without,with}_an` | end-to-end `resource.new` + `resource.drop` under AN, returning the handle index. Guards `translate_resource_drop`'s hand-written trampoline decoding of its i32 handle index; before the fix the encoded handle reached the host as "unknown handle index 65521" (`A·1`). |
| `refuse_atomic_{load,store,rmw_add,rmw_cmpxchg,fence}_under_an` / `refuse_memory_atomic_{notify,wait32}_under_an` | each compiles a wat module using a representative threads-proposal atomic operator and checks that compilation fails with "AN-encoding" in the message |
| `memory32_address_codeword_check_traps` | with memory32 and AN on, the encoded i32 address is checked before it's decoded for bounds/address math; corrupting the address global to a non-codeword traps with `AnCodewordInvalid` before the memory access happens |
| `memory64_with_an_is_allowed_and_encoded` / `memory64_address_codeword_check_traps` | with memory64 and AN on, i32/i64 store/load round-trips work through encoded i64 addresses, including nonzero/unaligned offsets; corrupting the encoded i64 address traps with `AnCodewordInvalid` before the memory access happens |
| `instantiate_data_segment_under_an` | a smoke test: AN-encoding shadow initialization doesn't panic when a data segment is present at instantiation |
| `fault_inject_flip_in_raw_traps` / `fault_inject_flip_in_shadow_traps` / `subword_store_checks_old_shadow_codeword` | flipping a bit in raw memory (untracked, via `Memory::data_ptr` — `data_mut` would mark it whole-dirty and get legitimately resynced), or in the encoded shadow (`an_shadow_data_mut_for_test`), after instantiation: a host `Memory::read` of the tampered slot fails its check when read (and reports the AN mismatch in the error message, not a generic "out of bounds"). The subword-store regression test corrupts an old shadow slot and checks the byte read-modify-write path traps with `AnCodewordInvalid` before decoding/merging it. |
| `try_data_traps_on_tamper` / `try_data_mut_traps_on_tamper` / `try_data_clean_passes` | the fallible `Memory` twins: a pre-existing raw/shadow mismatch makes `try_data`/`try_data_mut` return `Err(Trap::AnMemoryMismatch)` (where `data`/`data_mut` would panic); `try_data_mut` checks before marking the memory whole-dirty; the clean case returns `Ok` with the live bytes |
| `global_try_get_clean_passes` / `global_try_get_invalid_codeword_traps` / `global_get_panics_on_invalid_codeword` | host-boundary `Global::get` codeword validity: a slot corrupted to a non-multiple of `A` (injected via `an_corrupt_i64_slot_for_test`) makes `try_get` return `Err(Trap::AnCodewordInvalid)` and makes `get` panic; the clean case round-trips fine |
| `component_an::try_as_le_slice_clean_and_tamper` / `component_an::as_le_slice_panics_on_tamper` | the fallible `WasmList` twin: a `list<u32>` lifted from core memory reads back clean via `try_as_le_slice`; tampering a raw byte in the list's range makes `try_as_le_slice` return `Err(Trap::AnMemoryMismatch)`, while `as_le_slice` panics |
| `fault_inject_various_an_constants` | the fault-injection check fires for every legal `A` (1, 7, 1009, 65521, 2²⁴ − 1) |
| `fault_inject_clean_run_passes` | the sanity counterpart: a clean AN program with a host call runs without a false trap and returns 0 |
| `unaligned_i32_store_every_offset` | `i32.store` at every byte offset 0..7 with a 4-byte value; byte read-backs check both the raw bytes and (via the load-side check) the shadow |
| `cross_slot_i32_store16_every_offset` | `i32.store16` at every byte offset 0..7, exercising both the in-slot (`byte_pos in 0..=2`) and cross-slot (`byte_pos == 3`) paths |
| `unaligned_store_then_aligned_store_same_slot` | an aligned `i32.store` overwriting a slot that was previously touched by an unaligned byte read-modify-write, checks the slot stays a valid `A * u32` codeword |
| `bulk_wat_compiles_{without,with}_an` | a smoke test: a module exercising `memory.fill/copy/init/grow/size` plus `i32.store8/load` compiles cleanly under both AN modes |
| `bulk_memory_fill_keeps_shadow_consistent` | `memory.fill` over aligned, unaligned, and cross-slot ranges; byte read-backs check the shadow (load-side check) |
| `bulk_memory_copy_keeps_shadow_consistent` | non-overlapping and overlapping `memory.copy`; checks `memmove`-style overlap handling |
| `active_data_segment_keeps_shadow_consistent` / `passive_memory_init_keeps_shadow_consistent` | an active data segment mirrored into the shadow at instantiation, and `memory.init` of a passive segment kept consistent |
| `bulk_memory_grow_keeps_shadow_consistent` | `memory.grow` preserves a pre-grow sentinel byte, and the freshly grown pages encode as zero |
| `grow_does_not_resync_shadow_from_raw` / `grow_preserves_shadow_across_repeated_grows` | shadow-growth regression guards: a raw/shadow mismatch introduced before a grow must still be caught after it, by a guest `i32.load` of that slot (i.e. `memory.grow` must not re-encode the shadow from raw — that's the cause of the `big-strings` over-allocation bug), and written data must survive repeated grows with the load read-backs still matching |
| `bulk_memory_with_various_an_constants` | a bulk-op plus read-back check loop, across `A` ∈ {1, 7, 1009, 65521, 2^24−1} |
| `codeword_check::codeword_check_clean_wasm_to_host_args` / `codeword_check_clean_wasm_to_host_multi_args` / `codeword_check_clean_wasm_to_host_no_i32_params` / `codeword_check_clean_host_to_wasm_returns` / `codeword_check_clean_repeated_host_calls` / `codeword_check_clean_various_an_constants` / `codeword_check_no_trap_when_an_off` | positive coverage for the boundary codeword check. Every shape of wasm/host trampoline (one/many i32 args, no i32 args, return-only, many calls, every legal `A`) completes without a false trap. The AN-off counterpart checks the check is correctly gated off. |
| `codeword_check::codeword_check_traps_wasm_to_host_args_with_injection` / `codeword_check_traps_host_to_wasm_returns_with_injection` / `codeword_check_traps_various_an_constants` | negative coverage for the boundary codeword check. With `Config::an_inject_codeword_fault(1)` set, the trampoline bumps the first encoded i32 arg/result by 1 before the modulo check runs; the check is guaranteed to trap with `Trap::AnCodewordInvalid` for any `A > 1`. Covers both directions (wasm→host args, host→wasm returns) and several `A` values. |
| `component_codeword::component_i32_arg_passthrough_without_an` / `component_i32_arg_passthrough_with_an` / `component_i32_multi_arg_with_an` / `component_i32_various_an_constants` / `component_codeword_check_traps_with_injection` | components with `u32`-typed imports round-trip correctly under AN (single arg, multi arg, every legal `A`). The AN-off baseline checks the wat is well-formed. The fault-inject negative case checks the boundary codeword check fires on the component hostcall trampoline just like the core path. |
| `conversions::conversions_without_an` / `conversions_refused_under_an` | the float-containing `an_encoding/conversions.wat` runs end-to-end as an AN-off baseline (including wasm-spec trap behavior of `i32.trunc_f*_s/u`: NaN → `BadConversionToInteger`; ±∞, out-of-range, or a negative value into unsigned → `IntegerOverflow`); under AN it must be refused with a "floating-point" message |
| `int_conversions::int_conversions_{without,with}_an` / `int_conversions_with_various_an_constants` | the float-free `an_encoding/int_conversions.wat` (`i32.extend8_s/16_s`, `i32.wrap_i64`, `i64.extend_i32_s/u`) produces identical results with AN on and off. Edge cases: sign-extend bit boundaries (0x7F/0x80/0xFF), wrapping `i64::MAX/MIN` and `0x1_0000_0000`, and `extend_i32_u` of negative values. `_various` re-runs for `A ∈ {1, 7, 1009, 65521, 2^24 − 1}`. |
| `dirty_resync::shadow_tamper_during_hostcall_detected_on_read` / `memory_write_does_not_heal_unrelated_tamper` / `memory_write_during_hostcall_resyncs_written_range` / `unaligned_memory_write_resyncs_boundary_slots` / `memory_write_outside_hostcall_does_not_trap` / `data_mut_during_hostcall_resyncs_whole_memory` / `data_mut_does_not_heal_other_memory_tamper` / `dirty_resync_various_an_constants` | tests for the dirty-driven resync rules. A shadow tamper introduced *during* a host call survives the dirty-driven resync — it isn't silently healed — and is caught by a later host `Memory::read` of that slot. `Memory::write` re-encodes exactly the slots it wrote (an unrelated tamper elsewhere is still caught on a later read), works outside host calls too (a semantics change), and rounds outward to slot boundaries. `data_mut` writes resync through the whole-dirty flag, scoped only to the borrowed memory (multi-memory isolation: an untracked tamper on the *other* memory still survives). `_various` re-runs the core matrix for `A ∈ {1, 7, 1009, 65521, 2²⁴ − 1}`. |
| `component_an::string_lowering_then_host_boundary_{without,with}_an` / `string_lowering_then_host_boundary_various_an_constants` | host→wasm string-argument lowering writes raw bytes via the canonical ABI (`LowerContext`); the write-site re-encode has to keep the shadow consistent so the guest reads back the lowered bytes without a load-side trap. Also checks repeated calls stay consistent. |
| `wasi_fdstat_disjoint_same_slot_writes_resync` / `component_an::component_lowering_disjoint_same_slot_writes_{resync,reject_corrupt_padding}` / `dirty_resync::multi_range_resync_preserves_untouched_byte_check` | disjoint host-written ranges that share one AN slot are checked as a combined range before any re-encode, avoiding false corruption reports while still catching and preserving evidence of real corruption in untouched padding bytes. |
| `crates/wiggle/tests/an_dirty.rs` (7 tests) | unit coverage of the wiggle `GuestMemory` write-range recorder: typed writes (including float/pointer delegation to the integer version), `copy_from_slice`, `as_slice_mut`, coalescing adjacent writes, the bounded-list collapse on overflow, the untracked constructor recording nothing, and failed (out-of-bounds) writes recording nothing. |
| `crates/wiggle/tests/an_read.rs` (6 tests) | unit coverage of the wiggle `GuestMemory` read check done right before use: a clean typed read passes; a tampered slot read via `read` / `as_slice` / `to_vec` returns `AnMemoryMismatch`; a slot the same call wrote is skipped (no false trap); and a non-checking `unshared_an_tracked` view (no shadow) doesn't catch the mismatch. Built with TDD — it failed against the stub `an_cross_check_read` first, and passed once the slot-compare was in place. |
| `grow_then_store_same_function_reloads_shadow_base` | a regression guard: the shadow-base load isn't `readonly` — stores after a `memory.grow` (both straddling-block and loop shapes) must mirror into the *new* shadow buffer. The old flag was harmless with the current cranelift (load motion also needs `can_move`), but was one optimizer change away from a use-after-free bug |
| `host_memory_grow_keeps_shadow` | the embedder-facing `Memory::grow` now grows the shadow too (it used to leave the raw memory and the shadow at different sizes); a guest load read-back of a grown page exercises the grown shadow |
| `runtime::vm::instance::tests::an_shadow_resize_grows_capacity_geometrically` | shadow growth keeps the already-encoded part, zeroes the new logical tail, doubles the backing capacity when needed, and keeps the base pointer stable while later grows still fit in that reserved capacity |
| `data_mut_outside_hostcall_does_not_trap` | `Memory::data_mut` outside a host call is a legitimate write: the wasm-entry heal re-encodes the whole-dirty memory before the guest load runs, so the load reads the written byte instead of falsely tripping on a stale shadow |
| `memory64_mixed_copy_len_decodes` | `memory.copy` with a memory64 destination and a memory32 source: the i32-typed `len` (the *smaller* of the two index types) is decoded — the gate checks both memories |
| `simd_refused_under_an` / `gc_ops_refused_under_an` / `exceptions_refused_under_an` / `explicit_simd_enable_conflicts_with_an` / `winch_strategy_refused_under_an` | the feature mask refuses SIMD/GC/exception modules under AN; explicitly enabling one of these masked features, or picking the Winch strategy, alongside AN is a config error |
| `component_core_module_float_refused_under_an` | component core modules go through the same AN validation as plain core modules (so floats are refused there too) |
| `memory_copy_source_tamper_traps` | **host-read check right before use (memory.copy source).** A consistent source region is filled, then a source byte is tampered via the untracked `data_ptr` path; `memory.copy` to a disjoint destination must trap with `AnMemoryMismatch` at the source check — before it could copy the corruption into a valid-looking destination codeword (which is exactly what happened before the fix: the test failed with `got Ok`). Clean copies (`bulk_memory_copy_keeps_shadow_consistent`) still pass. |
| `component_an::component_lift_clean_run_passes` / `component_lift_tamper_traps` | **host-read check right before use (component lifting).** A core module's `start` writes `"hello"` at offset 16 (mirrored into the shadow); a host import `sink(string)` lifts it. Clean case: the host gets `"hello"` with no false trap. Tamper case: a raw byte flipped via `data_ptr` (reached through the new `Instance::an_core_memory_for_test`) makes the lift trap with `AnMemoryMismatch`. This failed with `got Ok` before the fix. |
| `data_mut_whole_verify_detects_pre_existing_corruption` | **`Memory::data_mut`'s check before borrowing.** A raw byte tampered via `data_ptr` before a `data_mut` borrow must be caught by the whole-memory check that runs *before* the borrow's re-encode (which could otherwise hide the tamper). This is a panicking accessor, so it's checked via `#[should_panic(expected = "AnMemoryMismatch")]`. |
| `crates/wasmtime` lib `runtime::memory::tests::an_cross_check_if_contains_ptr_detects_tamper` | **transcoder source-check building block (unit test).** Directly exercises `Memory::an_cross_check_if_contains_ptr`: a clean range gives `Some(true)`, a pointer that's out of range gives `None`, and a `data_ptr`-tampered range gives `Some(false)`. The full end-to-end transcode path is covered by `transcode_string_roundtrip_*` (clean, no false positive). |

Both AN modes must produce identical results, except where a feature
is refused under AN — in which case the AN-on run must fail to compile.


---

## Demo commands

Build the CLI first: `cargo build -p wasmtime-cli` (the binary lands at `./target/debug/wasmtime`).

### Build the fib demo (Rust → wasm32-wasip1)

```
cd ./an_encoding && rustc --target=wasm32-wasip1 -C opt-level=3 fib.rs && cd ..
```

### Run fib under AN

```
WASMTIME_LOG=warn ./target/debug/wasmtime run --dir . -C an-encoding=y -C cache=n an_encoding/fib.wasm
```

You can run any wasm module (as long as it stays within the AN-supported subset) with AN-encoding on:

```
./target/debug/wasmtime run --dir . -C an-encoding=y path/to/your/program.wasm
```

The same module runs without AN if you just drop `-C an-encoding=y`.

### Run with a custom A

```
./target/debug/wasmtime run --dir . -C an-encoding=y -C an-constant=1009 -C cache=n an_encoding/fib.wasm
```

### Compare generated code, AN on vs off

Compile a wat module both ways and diff the machine code / CLIF:

```
mkdir -p /tmp/demo

./target/debug/wasmtime compile -C an-encoding=y --emit-clif /tmp/demo/clif_on \
    -o /tmp/demo/mul_on.cwasm an_encoding/mul.wat
./target/debug/wasmtime compile --emit-clif /tmp/demo/clif_off \
    -o /tmp/demo/mul_off.cwasm an_encoding/mul.wat

./target/debug/wasmtime objdump --funcs all /tmp/demo/mul_on.cwasm
./target/debug/wasmtime objdump --funcs all /tmp/demo/mul_off.cwasm
```

`an_encoding/ops.wat` (one export per operator) works the same way, and is the
quickest place to look at the per-op changes described in *Per-op behaviour*.

### See a refusal

```
./target/debug/wasmtime compile -C an-encoding=y an_encoding/conversions.wat -o /dev/null
```

fails with the floating-point refusal message (the module runs fine without AN).

### Run the test suites

```
cargo test -p wasmtime-cli --test all an_encoding::
cargo test -p wiggle an_
```
