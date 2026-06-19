# AN-Encoding Changelog & additional information

This fork aims to implement AN-encoding for wasmtime. In the following you can find:
- The changed files and what was changed
- Implementation details of AN-encoding
- Design decisions
- Tests
- Demo commands

The implementation is heavily based on the paper Fetzer, Schiffel, Süßkraut, *AN-Encoding Compiler*, 2009.

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

`A` defaults to `wasmtime_environ::DEFAULT_AN_CONSTANT` (`65521`, a 16-bit
prime recommended by the paper). The setter validates `1 ≤ A < 2²³` (A=0 would
be impossible to decode and cause further issues [don't even think about it!]; A ≥ 2²³ would let LUT entries `A · 0xFF` overflow `i32`).
Pick a large odd value (preferably prime), powers of two weaken detection.

---

## Changes



### Runtime — `crates/wasmtime`

- **`runtime/an_lut.rs`** *(new)* — generates the per-`A` 256×256 `i32` and/or/xor LUTs (`A·(c1 OP c2)`, 256 KiB each).
- **`engine.rs`** — `EngineInner` owns the `AnLuts`; `an_lut_addr(op)` exposes table addresses (process-portable JIT).
- **`runtime.rs`** — registers the `an_lut` module.
- **`runtime/vm/instance.rs`** — per-instance AN state (LUT slots, `an_enc_shadows`, `an_whole_dirty` flags), the shadow encode / range-re-encode / lazy-grow routines (incl. imported-memory shadow access and `memory_grow` shadow growth), and the range/whole-memory cross-checks (`an_cross_check_range` / `an_cross_check_memory`) plus the `an_is_whole_dirty` peek that lets reads skip legitimately-stale memories.
- **`runtime/vm/instance/allocator.rs`** — mirrors data-segment / CoW init into each shadow before wasm starts.
- **`runtime/memory.rs`** — write hooks (`write` re-encodes its range; `data_mut`/`data_and_store_mut` mark the memory whole-dirty) and read-side verify-at-use: `read` cross-checks its exact range, `data`/`data_mut` cross-check the whole memory at borrow — the infallible accessors *panic* on a mismatch, the fallible `try_data`/`try_data_mut`/`try_data_and_store_mut` twins return `Err(Trap::AnMemoryMismatch)`. `Memory::read`'s `MemoryAccessError` names an AN mismatch rather than a generic "out of bounds". Plus `#[doc(hidden)]` shadow / cross-check accessors for the lifting, transcode-source, and wiggle paths, and a test-only shadow accessor.
- **`runtime/store.rs`** — `an_all_instance_ids()` (all instances incl. dummy host-memory owners) drives the store-wide dirty sweep; `an_heal_whole_dirty()` runs that sweep at the host→wasm entry chokepoint (`invoke_wasm_and_catch_traps`) so a `data_mut` between top-level calls is re-encoded before the guest's mandatory load-check runs.
- **`runtime/vm/libcalls.rs`** — `an_resync_host_boundary` libcall (emitted after a host call) dirty-sweeps `data_mut`-marked memories via `an_sweep_whole_dirty`. `memory.copy` cross-checks its source range before copying (so the destination re-encode cannot launder source corruption) and `memory.copy`/`memory.init` re-encode their written destination range after.
- **`runtime/component/func/{options,host}.rs`, `func.rs`, `func/typed.rs`, `values.rs`, `concurrent/futures_and_streams.rs`** — Lowering: `LowerContext` records host-written ranges (`get`/`slice_mut` exact, `as_slice_mut` whole-memory) and `an_flush_dirty` re-encodes them before control re-enters wasm. Lifting: `LiftContext` carries the options memory's shadow + `A` and cross-checks every lifted range (`memory_checked`, plus the lazy `WasmStr::to_str` / `WasmList::as_le_slice` accessors); all list/string/record/map/param lift sites in `func/typed.rs`, `func/host.rs` and `values.rs` route through it. (**Note:** streams and futures are not supported. The changes are relics of an attempt.)
- **`runtime/component/instance.rs`, `runtime/vm/component.rs`** — per-`RuntimeMemoryIndex` AN identity map + lookups for the lowering flush and transcoder resync; `an_options_shadow` / `an_options_whole_consistent` (lifting + `as_slice_mut` verify) and `an_check_transcode_src` (transcode source verify); `an_core_memory_for_test` exposes a component's core memory for fault-injection tests.
- **`runtime/vm/component/libcalls.rs`** — each string transcoder **cross-checks its source range** (`an_check_transcode_src`) before transcoding and re-encodes its written destination range after.
- **`runtime/externals/global.rs`** — host-boundary i32 global encode/decode (`Global::get`/`set`), gated to wasm i32 globals. `Global::get` verifies codeword validity (`enc % A == 0`) before decoding: the infallible `get` *panics* on an invalid codeword, the fallible `try_get` returns `Err(Trap::AnCodewordInvalid)`. `an_corrupt_i64_slot_for_test` injects an invalid codeword for tests.
- **`runtime/trampoline/global.rs`** — encodes the initial value of a host-created (`Global::new`) i32 global.
- **`runtime/vm/vmcontext.rs`** — `VMGlobalDefinition::{from,to}_val_raw` encode/decode i32 `ValRaw` ↔ storage.
- **`compile.rs`** — `validate_an_encoding_constraints` (core modules + component cores): refuse shared memory / float / atomics / reference-type *values* (signatures, globals, locals) / non-`funcref` tables, warn on memory64 + i32↔i64; SIMD/GC/exceptions/stack-switching/component-async refused via the `config.rs` feature mask. `funcref` tables stay allowed (`call_indirect` dispatch).
- **`config.rs`** — `an_encoding`/`an_constant` setters, test fault-inject knobs, the AN feature mask (incl. `CM_ASYNC`/`CM_ASYNC_STACKFUL`), and Winch refusal.
- **`engine/serialization.rs`** — AN tunables included in cwasm compatibility validation.

### Wiggle — `crates/wiggle`

- **`src/lib.rs`, `src/guest_type.rs`, `generate/src/wasmtime.rs`** — `GuestMemory` carries an optional write-range recorder and (under AN) a read-only shadow slice + `A`: writes funnel through the integer `write` impl and record their range, and every host read (`GuestPtr::read`, `as_slice`/`as_cow`/`as_str`, `to_vec`) slot-compares its byte range against the shadow via `an_cross_check_read`, returning `GuestError::AnMemoryMismatch` on divergence (skipping slots the same call wrote). The generated WASI p1 wrapper hands the view the shadow (`Memory::an_untracked_data_shadow_and_store_mut`) and re-encodes the recorded write ranges after the host body. `guest_error.rs` gains the `AnMemoryMismatch` variant.
- **`test-helpers/src/lib.rs`** — construct via the `GuestMemory::unshared(...)` ctor.
- **`tests/an_dirty.rs`** *(new)* — unit tests for the write-range recorder.
- **`tests/an_read.rs`** *(new)* — unit tests for the read verify-at-use slot-compare.

### WASI — `crates/wasi`, `crates/wasi-common`

- **`wasi/src/p1.rs`, `wasi-common/src/snapshots/preview_1/error.rs`** — map `GuestError::AnMemoryMismatch` to a trap in `From<GuestError>` (both the preview1 ABI-marshalling path and a user trait body), so an AN read mismatch is fatal rather than surfaced as an errno.

### Environment — `crates/environ`

- **`vmoffsets.rs`** — three LUT pointer slots in `VMContext`, per-memory `defined_memories_enc_bases` array, `VMMemoryImport::an_enc_base_slot` (pointer to the owner's enc-base slot; address stable across `memory.grow`).
- **`builtin.rs`** — declares the `an_resync_host_boundary` builtin (`-> bool`; falsy = trap).
- **`trap_encoding.rs`** — new `Trap::AnMemoryMismatch` (48) / `AnCodewordInvalid` (49); `c-api/src/trap.rs` const-asserts updated to match.
- **`tunables.rs`** — `DEFAULT_AN_CONSTANT = 65521`, `ENC_MEM_GROWTH_FACTOR = 2`, and the AN tunable fields.
- **`module.rs`** — `Module::an_raw_globals` records imported host-control globals (`InstanceFlags` / `TaskMayBlock`) as a raw↔encoded boundary.

### Cranelift — `crates/cranelift`

- **`lib.rs`** — `wasm_stack_value_type` widens i32→I64 under AN; `TRAP_AN_MEMORY_MISMATCH` / `TRAP_AN_CODEWORD_INVALID` codes.
- **`translate/an_helpers.rs`** — all AN codegen helpers: encode/decode, bitwise-LUT, mul, shifts/rotates, shadow-store RMW, the per-load validity check, and boundary / conversion codeword checks.
- **`translate/code_translator.rs`** — per-op AN encoding (see *Per-op behaviour*), shadow mirror for `i32.store{,8,16}`, i32 decode around `memory.*`/`table.*`/`call_indirect` and `br_table`, pass-through globals.
- **`func_environ.rs`** — widen i32 global storage to I64 (+ const-fold `iconst.i64 A·v`); raw host-control globals kept native with encode-on-get / decode-on-set.
- **`translate/func_translator.rs`, `translate/translation_utils.rs`** — widen the i32 local / block-param IR type under AN.
- **`translate/mod.rs`** — re-exports `emit_an_codeword_validity_check` for trampoline codegen.
- **`compiler.rs`** — wasm/host trampolines encode/decode i32 + boundary codeword check, and emit the `an_resync_host_boundary` post-call dirty-heal libcall.
- **`compiler/component.rs`** — same treatment for `translate_hostcall`; plus i32 decode/encode in the transcode, `resource_drop`, and `UnsafeIntrinsic` (load/store/context) trampolines.
- **`component/translate/adapt.rs`** — populates `an_raw_globals` when an adapter module is translated.

### CLI & tests

- **`crates/cli-flags/src/lib.rs`** — `-C an-encoding=y`, `-C an-constant=N`.
- **`an_encoding/`** *(new)* — wat/wasm modules used by the tests.
- **`tests/all/an_encoding.rs`** *(new)* — the AN test suite (see *Tests*).
- **`tests/all/main.rs`** — registers `mod an_encoding;`.

---

## Design

Representation inside an AN-encoded module:

| Wasm type | IR type | Holds |
|---|---|---|
| `i32` | `I64` | `A·v` with canonical `v ∈ [0, 2³²)` |
| `i64`, refs, v128 | unchanged | passed through, not encoded |

`i64` support could be added in the future. Floating-point types (`f32`/`f64`)
are refused outright when AN-encoding is on (see *Supported and unsupported
features*).

**Memory model:** two linear memories, one mirroring the other but encoded

Everything done with the linear memory is unchanged, but mirrored and encoded in a shadow as well, when AN-encoding is turned on, so there are two memories resulting in a 3x memory increase.

**Shadow verification/resync:** Correctness is checked only for relevant ranges before reading, slot-by-slot (`[slot] == A * u32_le(raw[4i..4i+4])`, one multiply + compare)

| Read path | Check |
|---|---|
| guest `i32.load{,8,16}` | inline per-slot check at the load site (mandatory under AN). Mismatch → `Trap::AnMemoryMismatch`. |
| host `Memory::read` | cross-checks exactly `[offset, offset+len)` before copying (`an_range_consistent`). Mismatch → `Err(MemoryAccessError)`; its typed return can't carry the `AnMemoryMismatch` trap *code*, but the error is enriched to name the AN mismatch (not a generic "out of bounds"). |
| host `Memory::data` / `data_mut` | opaque whole-memory borrow → cross-checks the whole memory at borrow. Infallible: *panics* on mismatch. Fallible twins `try_data`/`try_data_mut`/`try_data_and_store_mut` → `Err(Trap::AnMemoryMismatch)` (`try_data_mut` cross-checks before marking whole-dirty). |
| host `WasmList::as_le_slice` | cross-checks the list's bytes at borrow. Infallible: *panics*; fallible twin `try_as_le_slice` → `Err(Trap::AnMemoryMismatch)`. |
| host `Global::get` (i32) | codeword validity check (`enc % A == 0`) before decode. Infallible: *panics* on an invalid codeword; fallible twin `try_get` → `Err(Trap::AnCodewordInvalid)`. |
| component lifting (`LiftContext`) | per-range cross-check on every lifted value (`memory_checked` for the `cx` paths; `an_options_shadow` for the lazy `WasmStr::to_str` / `WasmList::as_le_slice`). Mismatch → `Trap::AnMemoryMismatch`. |
| component transcoders (fused string adapters) | source range cross-checked before transcoding (`an_check_transcode_src`); destination re-encoded after. |
| `memory.copy` | source range cross-checked before the copy (so the destination re-encode cannot launder source corruption). |
| WASI preview1 (wiggle reads) | exact-range cross-check in the `GuestMemory` view (`GuestPtr::read`, `as_slice`/`as_cow`/`as_str`, `to_vec`) against a read-only shadow handed out at hostcall entry; skips slots the same call already wrote. Surfaces `AnMemoryMismatch`, which `wasmtime-wasi` / `wasi-common` map to a **trap**. |
| host `Memory::data_ptr`, raw escape hatches | **not checked** — raw pointer, no range / lifetime (by design). |

Host writes are tracked per path and the shadow is re-encoded for exactly what the host wrote:

| Host write path | Shadow maintenance |
|---|---|
| `Memory::write` | immediate exact-range re-encode at the write site (also works *outside* host calls — see semantics changes) |
| `Memory::data_mut` / `data_and_store_mut` | whole-dirty flag on the memory; consumed (full re-encode + clear) at the next heal — host-call entry/exit *and* the host→wasm **entry** sweep (`an_heal_whole_dirty`), so a `data_mut` write between top-level calls is re-encoded before the guest's mandatory load-check runs (else it would false-trap on the stale shadow) |
| WASI preview1 (wiggle) | `GuestMemory` records every written byte range (typed `write`, `copy_from_slice`, `as_slice_mut`); the generated hostcall wrapper drains the ranges after the host body returns and re-encodes exactly those bytes |
| component canonical ABI (incl. WASI preview2) | `LowerContext` records ranges (`get` / `slice_mut` exact; raw `as_slice_mut` falls back to whole-memory) and flushes them with an immediate re-encode before control re-enters wasm: at `realloc` entry, after host→wasm argument lowering (`with_lower_context`), after host-result lowering (`lower_result_and_exit_call`), and at the async stream/future lowering sites |
| component transcode libcalls (fused adapters) | each transcoder re-encodes its destination range; the raw `dst` pointer is resolved back to the owning memory via the per-`RuntimeMemoryIndex` identity map captured at `extract_memory` |
| raw `Memory::data_ptr` writes | **not tracked**, indistinguishable from corruption. Can be used for fault injection though. |

Some design decision regarding the memory:
- The additional shadow has 2x the size of the regular memory
- Checking for correctness is only done for the relevant segments (some exceptions, see above): per-slot at every guest load and host read; never a periodic whole-memory sweep
- Wasm-side stores keep the shadow in lockstep via the JIT mirror; host-side writes re-encode the touched ranges as described above
- Unaligned/subword accesses use RMW like paper
- Shared/atomic memories are refused when AN-encoding is on; multi-memory is supported. Imported (non-shared) memories are supported: the importing instance's JIT code reaches the owner's shadow through the stable `VMMemoryImport::an_enc_base_slot` pointer (one extra load), so an owner-side `memory.grow` — which re-allocates the shadow — stays transparent to importers
- 64 bit memory operations are allowed, but not affected by the encoding, a warning is emitted

**Function signatures:** Internal wasm function sigs are widened: every wasm
`i32` param/result becomes IR `I64`. Trampolines convert at the wasm/host
boundary so external observers (host functions, the embedder) keep seeing
raw `i32`.

**AN-encoding injection:** The operations are modified when wasm is being translated to CLIF (Cranelift intermidiate representation), since information from wasm is needed (since we can't encode wasm linear memory, we need to know which memory accesses do what [to know if we can encode them], which would be lost in later stages)

**Canonical invariant:** After every operation, encoded i32 values are
brought back to `A·v with v ∈ [0, 2³²)`. This is what lets compares,
addresses, and host-call args work directly on the encoded form without
needing per-use decode.

### Supported and unsupported features

| Feature | Status under AN-encoding |
|---|---|
| `i32` | everything based on i32 should work, excepting conversions to not supported types and atomics, except i64 (see *Per-op behaviour*) |
| linear memory (incl. multi-memory, bulk-memory, `memory.grow`) | encoded shadow, verified at use (per-slot on guest loads + host reads) |
| tables, `call_indirect` | i32 index/length operands decoded around the builtin |
| `i64` | allowed, but outside of encoding, warning emitted |
| imported (non-shared) memories | stores mirror through the owner's shadow via `VMMemoryImport::an_enc_base_slot` |
|floats|**refused** at compile time
|SIMD|**refused** at compile time
| shared (atomic) memories, atomic operators | **refused** at compile time |
| reference types as *values* (externref, funcref-as-value, …) in signatures / globals / locals | **refused** at compile time (opaque host handles, no integer encoding) |
| non-`funcref` table element types (externref / GC / exn / cont tables) | **refused** at compile time |
| component-model async (futures / streams) | **refused** via the feature mask under AN |
| GC / exceptions / stack switching proposals | **refused** via the feature mask under AN |
| Winch | **refused** at config validation |
|wmemcheck| nothing implemented yet, so probably breaks

### Per-op behaviour

| Op | Strategy |
|---|---|
| `i32.const k` | emit `iconst.i64 (A·k)` |
| `i32.add` | `iadd` then canonicalize via overflow-check: `sum >= A·2³² ? sum - A·2³² : sum` |
| `i32.sub` | `isub` then canonicalize via underflow-check: `diff < 0 ? diff + A·2³² : diff` |
| `i32.mul` | `(P_hi, P_lo) = (umulhi, imul)(A·n, A·m) → udiv_u128_by_u64_const(·, A) → umod_u128_by_u64_const_to_i64(·, A·2³²)`. See *i32.mul* note below |
| `i32.div_u` | `(arg1 udiv arg2) · A` (one A naturally cancels) |
| `i32.rem_u` | unchanged: `A·n urem A·m = A·(n urem m)`  |
| `i32.div_s` | sign detected via `enc ≥ A·2³¹`, encoded absolute via `aw − enc` (with `aw = A·2³²`), `udiv` on absolutes (A cancels), re-encode `· A`, re-apply result sign (`s1 ⊕ s2`). Explicit `INT_MIN/-1 -> INTEGER_OVERFLOW` trap on encoded operands before the abs step. `/0` trap via `translate_udiv` on `abs2` (`abs2 = 0` iff `arg2 = 0`). Zero-quotient negation special-cased to preserve canonical form. |
| `i32.rem_s` | same sign-detect + abs trick, but uses `urem`, which preserves the `A` factor (`urem(A·\|n\|, A·\|m\|) = A·(\|n\| urem \|m\|)`), so no re-encode needed. Result takes the dividend's sign. `INT_MIN%-1` falls out as `urem(A·2³¹, A) = 0` (no trap, matches wasm). |
| `i32.eqz` | `icmp_imm Equal arg 0` produces an i8 boolean, then `select(bool, A, 0)` to encode as `0`/`A` |
| `i32.lt_u`, `le_u`, `gt_u`, `ge_u`, `eq`, `ne` | compare encoded operands directly (A preserves order + zero), then `select(bool, A, 0)` to encode the boolean result |
| `i32.lt_s`, `le_s`, `gt_s`, `ge_s` | remap each operand to `c' = (c + A·2³¹) mod (A·2³²)`, then unsigned compare |
| `i32.and`, `i32.or`, `i32.xor` | tabulated on functional 8-bit chunks via `emit_an_bitwise_i32` (like the paper). One `udiv` per operand decodes; four `(c1<<8)\|c2` indexes load `A·(c1 OP c2)` from a 256×256 `i32` table (zero-extended to `i64`), then `acc += entry << (8·i)` recombines to `A·(n OP m)`. Tables live on the `Engine` (per-A, generated by `crates/wasmtime/src/runtime/an_lut.rs`); their address is loaded from a fixed `VMContext` slot at op-site (`load.i64 [vmctx + offset]`), so the same machine code is portable across processes. |
| `i32.not` | wasm has no native `i32.not`; written as `i32.const -1; i32.xor` and follows the `i32.xor` LUT path. |
| `i32.shl` | decode count (`udiv enc_k, A`), mask `& 31`. Value stays encoded: helper `emit_an_shl_i32` computes `enc_v · 2^k`, then canonicalizes via the existing 128/64 `umod_u128_by_u64_const_to_i64` against `A·2³²`. |
| `i32.shr_u` | decode count, mask `& 31`. `udiv(enc_v, A·2^k)` cancels `A` out of the dividend naturally, giving raw `v >> k`; re-encode with `· A`. **Note:** Paper decodes count and uses it as index to LUT  |
| `i32.shr_s` | reuse `emit_an_shr_u_i32` for the logical part, then `iadd` an encoded sign-extension mask if `enc_v ≥ A·2³¹` (negative). Mask is `A·((1<<k)−1)·2^(32−k) = aw − (aw >> k_mod)` -> two instr., unlike paper's `signExt[]` table. Addition is exact because the logical shift result has top `k` bits clear. **Note:** same as above|
| `i32.rotl`, `i32.rotr` | `(v << k_mod) \| (v >> (32−k_mod))`,  bit ranges disjoint, so OR ≡ ADD on encoded sums. Implemented as `iadd(emit_an_shl_i32, emit_an_shr_u_i32)` with appropriate shift amounts. Both helpers support shift `[0, 32]`; at `k_mod = 0` the "complement" shift naturally returns 0 (shl(_, 32) ≡ 0 mod `aw`; shr_u(_, 32) ≡ 0 since `enc_v < aw`), so identity rotation falls out without special-case. |
| `i32.clz`, `i32.ctz`, `i32.popcnt` | Decode once (`udiv enc, A`), `ireduce.i32`, native op, `uextend.i64`, re-encode by `· A`. **Note:** impossible without decode, as it is bit-level inspection (afaik) |
| `i32.load{,8_u,16_u,8_s,16_s}` | for memory32 (i32 indices): decode addr (÷A → trunc.i32) → wasm load (raw) → `uextend.i64` → ·A. Loads pull from the raw buffer. **Verify-at-use (mandatory under AN):** an inline assertion (`enc_slot == A * u32_le(raw_slot)`) fires per touched shadow slot right after the raw load (after its bounds enforcement — so a guard-page-protected raw access traps first on OOB and the shadow heap buffer is never indexed out of bounds — but before the loaded value is used); mismatch → immediate `AnMemoryMismatch` trap. This is the guest-read half of verify-at-use (replaces the removed host-boundary cross-check). For memory64 the popped i64 address is raw and passes through; the loaded value is still encoded if the result type is i32. |
| `i32.store` (4-byte) | decode addr, decode value (÷A → trunc.i32); wasm store raw. **Plus** AN-encoded mirror: runtime branch on `effective_addr & 3`. Aligned path (`byte_pos == 0`) does a single `store.i64 [enc_base + 2*effective_addr]` of the encoded operand `A*v`. Unaligned path decomposes into four byte-RMWs at consecutive byte addresses; each helper computes its own slot index so cross-slot transitions fall out automatically. |
| `i32.store8` | decode addr, decode value; wasm store raw byte. **Plus** single byte-RMW on the shadow slot containing the target byte. `i32.store8` always fits in one slot. |
| `i32.store16` | decode addr, decode value; wasm store raw half. **Plus** two byte-RMWs at `effective_addr` and `effective_addr + 1`. Covers in-slot (`byte_pos in 0..=2`) and cross-slot (`byte_pos == 3`) cases uniformly because each byte-RMW computes its own slot index. |
| `i64.store{,8,16,32}` | decomposed into one or two raw i32 sub-stores (`translate_non_i32_store_an`), each mirrored into the shadow via the same aligned/unaligned dispatch as `i32.store{,8,16}` — keeps the shadow consistent for i64 data without encoding the i64 value itself |
| `v128.store` (and lane stores) | loud translation error under AN (`wasm_unsupported!`); unreachable in practice since SIMD is feature-masked off |
| `local.{get,set,tee}` (i32) | type widened to I64 by the sig/locals widening |
| `global.get` (i32) | i32 globals are stored encoded, so no per-access tranform is needed for the guest. Their `VMGlobalDefinition` storage type is widened to `I64` in `make_global` (the slot is 16 bytes, so there is room). Imports, defined globals, and constant-folded immutable globals all load the encoded form (constant-folded ones emit `iconst.i64 (A·v)` directly). Decoding happens only at external boundaries |
| `global.set` (i32) | the operand is already the canonical encoded `A·v` (`I64`), so no change is needed. Non-i32 globals pass through unchanged. Encoding/decoding happens only at external boundaries |
| `i32.extend8_s` / `i32.extend16_s` | stays inside the encoding. Decode (`udiv → ireduce.i32`, no codeword check because of structural invariant, matches `clz`/`ctz`/`popcnt`), sign-extend the low byte/half-word to i32, re-encode via `emit_an_encode_raw_i32` (`uextend.i64 → · A`). |
| `i32.wrap_i64` | raw i64 → encoded i32. Take low 32 bits (`ireduce.i32`), re-encode. Wasm-spec: no trap. Input is *not* a codeword (raw i64), so no codeword check. Compile emits a one-shot per-module warning ([Conversion warning](#conversion-warning)). |
| `i64.extend_i32_s` / `i64.extend_i32_u` | encoded i32 → raw i64. Boundary codeword check via `emit_an_conversion_decode_i32` (optionally bumps by `an_inject_conversion_fault` first), then `urem` + `trapnz` against `Trap::AnCodewordInvalid`, then decode `udiv A → ireduce.i32`, then `sextend`/`uextend` to `I64`. Output leaves the AN encoding; warning emitted at compile time. |
| `br_if` / `if` / `select` cond | unchanged |
| host-import call (wasm → host) | decode i32 args, encode i32 returns at the `wasm_to_array` trampoline. After the call the trampoline emits the `an_resync_host_boundary` libcall — **dirty-heal only** (re-encode memories the host borrowed wholesale via `Memory::data_mut`; whole-dirty flag). The old whole-memory cross-check that ran pre-call is removed — corruption is caught at use (guest load / host read) — and the pre-call libcall itself is gone (a no-op: nothing is whole-dirty entering a host call). Range-tracked host writes (`Memory::write`, wiggle, component lowering) re-encode their exact ranges at the write site. **Boundary codeword check** is emitted on every encoded i32 arg before the `udiv` decode: `val % A != 0 → Trap::AnCodewordInvalid`. |
| host → wasm entry call | encode i32 args, decode i32 returns at the `array_to_wasm` trampoline. **Boundary codeword check** is emitted on every encoded i32 result before the `udiv` decode. |




### `i32.mul` note

To implement `i32.mul` so that it stays encoded, the division uses algorithm 4 proposed in the paper "Improved Division by Invariant Integers", Möller & Granlund, 2010.
High level overview (see `crates/cranelift/src/translate/an_helpers.rs` for more details):
1. Calculate the raw product P  = (A·n) · (A·m) = A²·n·m
2. Calculate the quotient Q  = P / A = A·n·m
3. Canonicalize the result R = Q mod (A·2³²) = A·(n·m mod 2³²)

For this, several helper functions have been implemented.





### Conversion warning

When `tunables.an_encoding == true` and the module contains an `i32 ↔ i64`
conversion op (e.g. `i32.wrap_i64`, `i64.extend_i32_s/u`),
`validate_an_encoding_constraints` walks every function body once and emits a
single `log::warn!` per module: i32 values crossing these ops leave the AN
encoding and the resulting i64 operands are not AN-protected.
The implementation lives in
`crates/wasmtime/src/compile.rs::is_i32_i64_conversion_op`.

### Validity checks

Codeword-validity (`val % A == 0`) is checked at the wasm/host trampoline
boundaries (both directions — core-wasm trampolines in `compiler.rs` and the
component-model `translate_hostcall` path in `compiler/component.rs`) and at
the `i64.extend_i32_s/u` conversion decode sites. Additionally, it is checked in every operand that requires a decode (e.g. `clz`, `and`, subword/unaligned `i32.store`, ...) See *New traps* below.

Errors occurring during the decoding operation itself are not detected.

For memory validity checks (the per-read-path **Read path / Check** table) and
resync details, see *Shadow verification/resync* above.


### New traps

`Trap::AnMemoryMismatch` (variant `48`) is raised when an encoded
shadow slot disagrees with raw bytes:
- inline at the guest load site when using a `load` instruction on the memory, fires at the exact load that observed the divergence.
- See *Shadow verification/resync* above for further details on memory verification.

**Note:** There are new `try_*` variants for infallible accessors (`Memory::data`/`data_mut`,
`WasmList::as_le_slice`, `LowerContext::as_slice_mut`, `Global::get`), the original ones will now panic if a divergence is detected.

`Trap::AnCodewordInvalid` (variant `49`) is raised by the boundary codeword
validity check at every wasm/host trampoline decode site. Specifically:

- `compile_wasm_to_array_trampoline` emits the check on every encoded i32
  arg before the `udiv` decode (wasm caller invokes a host import).
- `array_to_wasm_trampoline` emits the check on every encoded i32 result
  before the `udiv` decode (host invokes wasm via the entry trampoline).
- the component hostcall trampoline (`translate_hostcall`) emits the check
  on every encoded i32 param before decode.
- every `i64.extend_i32_s/u` conversion decode site emits the check before
  taking the i32 out of the encoding.
- every op-internal decode site (see *Validity checks* above: `clz`, `and`,
  subword/unaligned `i32.store`, ...) emits the check on the encoded operand
  before the decoding `udiv`.
- the host boundary `Global::get` (i32) checks the encoded 64-bit slot before
  decode; `get` *panics* on an invalid codeword, `try_get` returns
  `Err(Trap::AnCodewordInvalid)`.





---

## Tests

The tests were generated with the help of AI.

```cargo test -p wasmtime-cli --test all an_encoding::``` 
group with AN off and on:

| Test | Coverage |
|---|---|
| `mul_{without_an,with_an}` | `i32.mul` end-to-end on the native backend |
| `fib_{without_an,with_an}` | `an_encoding/fib.wat` end-to-end via WASI preview1 (`MemoryInputPipe` / `MemoryOutputPipe`) |
| `fib_with_an_and_load_validity_check` | same fib run exercising the WASI `fd_read` → post-host resync → wasm load chain with the (now-mandatory) per-load check |
| `data_mut_between_calls_resynced_before_guest_load` | a legitimate `Memory::data_mut` write between top-level calls must be re-encoded at wasm entry before the guest's mandatory load-check runs, else it false-traps on a stale shadow (regression guard for the entry heal) |
| `ops_{without_an,with_an}` | one wat module exporting one function per touched operator: add, sub, mul, divu, remu, divs, rems, addconst, lt_u, ge_u, gt_u, eq, ne, eqz, lt_s/le_s/gt_s/ge_s, and/or/xor/not/mask_merge, shl/shr_u/shr_s/rotl/rotr, clz/ctz/popcnt, max_u, loop_count, digits, memory load/store, mutable i32 global (g_get/g_set/g_inc) plus negative immutable initializer. Shifts/rotations cover 12 value patterns × 14 shift counts (including wraparound > 32). Includes trap assertions for `div_s` (`/0`, `INT_MIN/-1`) and `rem_s` (`/0`, `INT_MIN%-1 → 0`). |
| `ops_with_an_custom_constants` | re-runs the `ops_*` assertions with several non-default values of `A` (1, 7, 1009, 2²³ − 1) to verify the codegen reads `A` from `Tunables` rather than baking the default in |
| `global_boundary_{without,with}_an` / `global_import_{without,with}_an` / `global_boundary_various_an_constants` | host-boundary global encode/decode. `global_boundary_*` exports a mutable and an immutable i32 global directly and cross-checks the host view (`Global::get`/`set`) against the guest view (`global.get`/`set`) over a value matrix (incl. negatives, `i32::MIN/MAX`); the host always sees raw values while storage stays encoded. `global_import_*` imports a host-created (`Global::new`) i32 global into the module, exercising the `VMGlobalKind::Host` storage path (host init + `set`/`get` + guest mutation round-trip). `_various` re-runs both under `A ∈ {1, 7, 1009, 2²³ − 1}`. AN-off counterparts confirm identical behavior. |
| `refuse_float_{param,result,local,global,op}_under_an` | a float in a function signature, global, local, or operator stream must fail compilation under AN with a "floating-point" message |
| `refuse_shared_memory_under_an` | compiles a shared-memory wat module under AN and asserts the error mentions AN-encoding |
| `imported_memory_compiles_under_an` / `imported_memory_stores_mirror_owner_shadow` / `imported_memory_tamper_{raw,shadow}_traps` / `imported_memory_bulk_ops_keep_shadow` / `imported_memory_grow_through_importer` / `imported_memory_various_an_constants` / `host_created_memory_imported_under_an` | imported-memory support matrix: an exporting instance owns the memory and the importer stores/loads/fills/copies/grows through it; verify-at-use covers the import (clean runs pass via guest-load read-backs; raw/shadow tampering is caught by a host `Memory::read` of the owner); a host-created `Memory::new` import works incl. the `Memory::write`/`data_mut` host-write paths; re-run across `A ∈ {1, 7, 1009, 65521, 2²³−1}` |
| `multi_memory_compiles_under_an` / `multi_memory_stores_keep_shadows_consistent` / `multi_memory_tamper_{mem0,mem1}_traps` | multi-memory module with two defined memories: stores route to each via `memarg.memory` (verified by guest-load read-backs), and tampering either memory's raw bytes is caught independently by a host `Memory::read` of that memory |
| `load_validity_check_clean_run_passes` / `load_validity_check_traps_on_{raw_tamper,load8u,load16u_cross_slot}` / `load_validity_check_traps_unaligned_i32_load` / `load_validity_check_various_an_constants` | mandatory per-load check: tampering raw bytes via the untracked `Memory::data_ptr` path (NOT `data_mut`, which would legitimately resync) between instantiation and a wasm load makes the load raise `AnMemoryMismatch` immediately. Covers `i32.load`/`load8_u`/`load16_u`, aligned + unaligned + cross-slot positions, and several A values. |
| `br_table_{without,with}_an` | a `br_table` with three explicit targets plus a default; confirms non-zero selectors 1/2 select their arm and out-of-range selectors (3, 7) hit the default, under AN-on and AN-off (selector 0 is omitted: `A*0 == 0` makes a missing decode invisible there). Regression guard: the controlling index is a raw i32 selector and must be decoded before `br_table`, otherwise the encoded value (`A*v`) lands out of range and every non-zero index silently falls through to the default. This is the one index-consuming operator the rest of the matrix did not cover. |
| `table_{size,grow,fill,copy,init}_under_an` / `call_indirect_under_an` / `table_ops_match_without_an` | a wat with a 4-element funcref table exercises each table op under AN-on and confirms behavior matches the AN-off baseline. Without the per-operand decode, encoded i64 operands flowing into `cast_index_to_i64` panic in cranelift. `call_indirect` covers the vtable dispatch case (the hot path for closures / virtual calls in real wasm). |
| `component_an::component_compiles_{without,with}_an` / `component_an::component_with_an_various_constants` | component-model integration: a component wraps a core module that does an `i32.store` and then calls a host import via canon-lower. The AN dirty-heal + resync libcalls fire from the component hostcall trampoline using the core caller's vmctx. The "various constants" case re-runs across `A ∈ {1, 7, 1009, 65521, 2^23 − 1}` to confirm the libcalls read `A` from the engine tunables. |
| `component_an::transcode_component_compiles_{without,with}_an` | compiles a component that transcodes a string between encodings (utf8 → utf16) under AN (constants 1, 7, 65521, 2²³−1). Regression guard for the string-transcoder trampoline: before the fix `uextend.i64` was applied to an already-encoded i64 ptr/len arg, panicking cranelift aarch64 lowering with `assert!(inner_bits < out_bits)`. |
| `component_an::transcode_string_roundtrip_{without,with}_an` | end-to-end: lowers a host `&str` into a component and reads back its UTF-8 byte length (ASCII `"hello"` → 5; multi-byte `"héllo"` → 6). Exercises the whole string-ABI path under AN: transcoder trampoline arg-decode/result-encode, the realloc call into AN-compiled core wasm, and the raw `may_enter`/`may_leave` instance-flag globals (encode-on-get / decode-on-set). Before the flag fix this trapped "cannot leave component instance". |
| `component_an::resource_new_drop_{without,with}_an` | end-to-end `resource.new` + `resource.drop` under AN, returning the handle index. Guards `translate_resource_drop`'s hand-written trampoline decoding its i32 handle index; before the fix the encoded handle reached the host as "unknown handle index 65521" (`A·1`). |
| `refuse_atomic_{load,store,rmw_add,rmw_cmpxchg,fence}_under_an` / `refuse_memory_atomic_{notify,wait32}_under_an` | each compiles a wat module exercising a representative threads-proposal atomic operator and asserts compilation fails with "AN-encoding" in the message |
| `memory64_with_an_is_allowed_with_warning` | memory64 + AN compiles (warning-only) |
| `instantiate_data_segment_under_an` | smoke test: AN-encoding shadow init does not panic when a data segment is present at instantiation |
| `fault_inject_flip_in_raw_traps` / `fault_inject_flip_in_shadow_traps` | flip a bit in raw memory (untracked, via `Memory::data_ptr` — `data_mut` would mark whole-dirty and be legitimately resynced) resp. in the encoded shadow (`an_shadow_data_mut_for_test`) after instantiation; a host `Memory::read` of the tampered slot fails its verify-at-use cross-check (and reports the AN mismatch in the error message, not a generic "out of bounds") |
| `try_data_traps_on_tamper` / `try_data_mut_traps_on_tamper` / `try_data_clean_passes` | fallible `Memory` twins: a pre-existing raw/shadow divergence makes `try_data`/`try_data_mut` return `Err(Trap::AnMemoryMismatch)` (where `data`/`data_mut` would panic); `try_data_mut` cross-checks before marking whole-dirty; the clean case returns `Ok` with the live bytes |
| `global_try_get_clean_passes` / `global_try_get_invalid_codeword_traps` / `global_get_panics_on_invalid_codeword` | host-boundary `Global::get` codeword validity: a slot corrupted to a non-multiple of `A` (injected via `an_corrupt_i64_slot_for_test`) makes `try_get` return `Err(Trap::AnCodewordInvalid)` and `get` panic; the clean case round-trips |
| `component_an::try_as_le_slice_clean_and_tamper` / `component_an::as_le_slice_panics_on_tamper` | fallible `WasmList` twin: a `list<u32>` lifted from core memory reads back clean via `try_as_le_slice`; tampering a raw byte in the list's range makes `try_as_le_slice` return `Err(Trap::AnMemoryMismatch)` while `as_le_slice` panics |
| `fault_inject_various_an_constants` | the fault-injection detection fires for every legal `A` (1, 7, 1009, 65521, 2²³ − 1) |
| `fault_inject_clean_run_passes` | sanity counterpart: a clean AN program with a host call runs without a spurious trap and returns 0 |
| `unaligned_i32_store_every_offset` | `i32.store` at every byte offset 0..7 with 4-byte value; byte read-backs verify raw bytes and (via the load-side check) shadow consistency |
| `cross_slot_i32_store16_every_offset` | `i32.store16` at every byte offset 0..7, exercising in-slot (`byte_pos in 0..=2`) and cross-slot (`byte_pos == 3`) paths |
| `unaligned_store_then_aligned_store_same_slot` | aligned `i32.store` overwriting a slot previously touched by an unaligned byte-RMW path, confirms the slot stays a valid `A * u32` codeword |
| `bulk_wat_compiles_{without,with}_an` | smoke test: a module exercising `memory.fill/copy/init/grow/size` plus `i32.store8/load` compiles cleanly under both AN modes |
| `bulk_memory_fill_keeps_shadow_consistent` | `memory.fill` over aligned + unaligned + cross-slot ranges; byte read-backs verify the shadow (load-side check) |
| `bulk_memory_copy_keeps_shadow_consistent` | non-overlapping and overlapping `memory.copy`; verifies `memmove`-style overlap handling |
| `active_data_segment_keeps_shadow_consistent` / `passive_memory_init_keeps_shadow_consistent` | active data segment mirrored into the shadow at instantiation, and `memory.init` of a passive segment kept consistent |
| `bulk_memory_grow_keeps_shadow_consistent` | `memory.grow` preserves a pre-grow sentinel byte and the freshly grown pages encode as zero |
| `grow_does_not_resync_shadow_from_raw` / `grow_preserves_shadow_across_repeated_grows` | shadow-grow regression guards: a raw/shadow divergence introduced before a grow must still be caught after it by a guest `i32.load` of the slot (i.e. `memory.grow` must not re-encode the shadow from raw, the `big-strings` over-allocation cause), and written data must survive repeated grows with the load read-backs still agreeing |
| `bulk_memory_with_various_an_constants` | bulk-op + read-back verify loop across `A` ∈ {1, 7, 1009, 65521, 2^23−1} |
| `codeword_check::codeword_check_clean_wasm_to_host_args` / `codeword_check_clean_wasm_to_host_multi_args` / `codeword_check_clean_wasm_to_host_no_i32_params` / `codeword_check_clean_host_to_wasm_returns` / `codeword_check_clean_repeated_host_calls` / `codeword_check_clean_various_an_constants` / `codeword_check_no_trap_when_an_off` | boundary codeword check positive coverage. Every wasm/host trampoline shape (one/many i32 args, no-i32, return-only, many calls, every legal `A`) completes without false-positive. AN-off counterpart confirms the check is gated correctly. |
| `codeword_check::codeword_check_traps_wasm_to_host_args_with_injection` / `codeword_check_traps_host_to_wasm_returns_with_injection` / `codeword_check_traps_various_an_constants` | boundary codeword check negative coverage. With `Config::an_inject_codeword_fault(1)` set, the trampoline bumps the first encoded i32 arg/result by 1 before the modulo check fires; the check is guaranteed to trap as `Trap::AnCodewordInvalid` for any `A > 1`. Covers both directions (wasm→host args, host→wasm returns) and several `A` values. |
| `component_codeword::component_i32_arg_passthrough_without_an` / `component_i32_arg_passthrough_with_an` / `component_i32_multi_arg_with_an` / `component_i32_various_an_constants` / `component_codeword_check_traps_with_injection` | components with `u32`-typed imports round-trip correctly under AN (single arg, multi arg, every legal `A`). AN-off baseline confirms the wat is well-formed. The fault-inject negative case confirms the boundary codeword check fires on the component hostcall trampoline like the core path. |
| `conversions::conversions_without_an` / `conversions_refused_under_an` | the float-containing `an_encoding/conversions.wat` runs end-to-end as an AN-off baseline (incl. wasm-spec trap behaviour of `i32.trunc_f*_s/u`: NaN → `BadConversionToInteger`; ±∞ / out-of-range / negative-into-unsigned → `IntegerOverflow`); under AN it must be refused with a "floating-point" message |
| `int_conversions::int_conversions_{without,with}_an` / `int_conversions_with_various_an_constants` | the float-free `an_encoding/int_conversions.wat` (`i32.extend8_s/16_s`, `i32.wrap_i64`, `i64.extend_i32_s/u`) produces identical results AN-on and AN-off. Edge cases: sign-extend bit boundaries (0x7F/0x80/0xFF), wrap from `i64::MAX/MIN` and `0x1_0000_0000`, `extend_i32_u` of negatives. `_various` re-runs for `A ∈ {1, 7, 1009, 65521, 2^23 − 1}`. |
| `dirty_resync::shadow_tamper_during_hostcall_detected_on_read` / `memory_write_does_not_heal_unrelated_tamper` / `memory_write_during_hostcall_resyncs_written_range` / `unaligned_memory_write_resyncs_boundary_slots` / `memory_write_outside_hostcall_does_not_trap` / `data_mut_during_hostcall_resyncs_whole_memory` / `data_mut_does_not_heal_other_memory_tamper` / `dirty_resync_various_an_constants` | dirty-driven resync semantics. A shadow tamper introduced *during* a host call survives the (dirty-driven) resync — it is not silently healed — and is caught by a later host `Memory::read` of the slot. `Memory::write` re-encodes exactly the written slots (an unrelated tamper elsewhere is still caught on read), works outside host calls (semantics change), and rounds outward to slot boundaries. `data_mut` writes resync via the whole-dirty flag, scoped to the borrowed memory only (multi-memory isolation: an untracked tamper on the *other* memory survives). `_various` re-runs the core matrix for `A ∈ {1, 7, 1009, 65521, 2²³ − 1}`. |
| `component_an::string_lowering_then_host_boundary_{without,with}_an` / `string_lowering_then_host_boundary_various_an_constants` | host→wasm string-argument lowering writes raw bytes via the canonical ABI (`LowerContext`); the write-site re-encode must keep the shadow consistent so the guest reads the lowered bytes back without a load-side trap. Also proves repeated calls stay consistent. |
| `crates/wiggle/tests/an_dirty.rs` (7 tests) | unit coverage of the wiggle `GuestMemory` write-range recorder: typed writes (incl. float/pointer delegation to the integer impl), `copy_from_slice`, `as_slice_mut`, coalescing of adjacent writes, the bounded-list collapse on overflow, the untracked constructor recording nothing, and failed (out-of-bounds) writes recording nothing. |
| `crates/wiggle/tests/an_read.rs` (6 tests) | unit coverage of the wiggle `GuestMemory` read verify-at-use: a clean typed read passes; a tampered slot read via `read` / `as_slice` / `to_vec` returns `AnMemoryMismatch`; a slot the same call wrote is skipped (no false-trap); and a non-verifying `unshared_an_tracked` view (no shadow) does not catch divergence. Built TDD — RED against the stub `an_cross_check_read`, GREEN once the slot-compare landed. |
| `grow_then_store_same_function_reloads_shadow_base` | regression guard: the shadow-base load is not `readonly` — stores after a `memory.grow` (straddling-block and loop shapes) must mirror into the *new* shadow buffer. The old flag was latent in current cranelift (load motion additionally requires `can_move`) but one optimizer change away from use-after-free |
| `host_memory_grow_keeps_shadow` | embedder-facing `Memory::grow` grows the shadow too (it used to leave a raw/shadow size mismatch); a guest load read-back of a grown page exercises the grown shadow |
| `data_mut_outside_hostcall_does_not_trap` | `Memory::data_mut` outside a host call is a legitimate write: the wasm-entry heal re-encodes the whole-dirty memory before the guest load, so the load reads the written byte instead of false-trapping on a stale shadow |
| `memory64_mixed_copy_len_decodes` | `memory.copy` with memory64 destination ← memory32 source: the i32-typed `len` (the *min* of the two index types) is decoded — the gate consults both memories |
| `simd_refused_under_an` / `gc_ops_refused_under_an` / `exceptions_refused_under_an` / `explicit_simd_enable_conflicts_with_an` / `winch_strategy_refused_under_an` | the feature mask refuses SIMD/GC/exception modules under AN; explicitly enabling a masked feature, or selecting the Winch strategy, alongside AN is a config error |
| `component_core_module_float_refused_under_an` | component core modules pass through the same AN validation as plain core modules (float refusal) |
| `int_conversions::int_conversions_codeword_check_traps_with_injection` / `int_conversions_codeword_check_traps_various_an_constants` / `int_conversions_no_codeword_trap_without_injection` | conversion boundary-codeword check coverage on `i64.extend_i32_s/u`. `Config::an_inject_conversion_fault(1)` bumps the encoded i32 by 1 at the decode site (harness funcs source their i32 from `i32.const`, so the conversion site is the first decode boundary), guaranteeing a `Trap::AnCodewordInvalid` for `A > 1`. `_various` covers `A ∈ {7, 1009, 65521, 2^23 − 1}`. The no-injection counterpart confirms no false-positive at any legal A. |
| `memory_copy_source_tamper_traps` | **host-read verify-at-use (memory.copy source).** A consistent source region is filled, then a source byte is tampered via the untracked `data_ptr` path; `memory.copy` to a disjoint destination must trap `AnMemoryMismatch` at the source cross-check — before the copy laundered the divergence into a valid destination codeword (which it did pre-fix: the test was RED with `got Ok`). Clean copies (`bulk_memory_copy_keeps_shadow_consistent`) still pass. |
| `component_an::component_lift_clean_run_passes` / `component_lift_tamper_traps` | **host-read verify-at-use (component lifting).** A core module's `start` writes `"hello"` at offset 16 (mirrored into the shadow); a host import `sink(string)` lifts it. Clean: the host receives `"hello"` with no false trap. Tamper: a raw byte flipped via `data_ptr` (reached through the new `Instance::an_core_memory_for_test`) makes the lift trap `AnMemoryMismatch`. RED pre-fix (`got Ok`). |
| `data_mut_whole_verify_detects_pre_existing_corruption` | **`Memory::data_mut` pre-borrow whole verify.** A raw byte tampered via `data_ptr` before a `data_mut` borrow must be caught by the whole-memory cross-check that runs *before* the borrow's laundering whole-re-encode. Infallible accessor → asserted via `#[should_panic(expected = "AnMemoryMismatch")]`. |
| `crates/wasmtime` lib `runtime::memory::tests::an_cross_check_if_contains_ptr_detects_tamper` | **transcoder source-check primitive (unit).** Directly exercises `Memory::an_cross_check_if_contains_ptr`: a clean range → `Some(true)`, an out-of-range pointer → `None`, a `data_ptr`-tampered range → `Some(false)`. The end-to-end transcode path is covered by `transcode_string_roundtrip_*` (clean, no false-positive). |

Both AN modes are required to produce identical results (except where a feature
is refused under AN, in which case the AN-on run must fail to compile).


---

## Demo commands

### Compile fib

```
cd ./an_encoding && rustc --target=wasm32-wasip1 -C opt-level=3 fib.rs && cd ..
```

### Run fib

```
WASMTIME_LOG=warn ./target/debug/wasmtime run --dir . -C an-encoding=y -C cache=n an_encoding/fib.wasm
```

### Compile + inspect (AN on vs off)

```
./target/debug/wasmtime compile -C an-encoding=y an_encoding/fib.wat -o /tmp/demo/fib.cwasm
```

```
./target/debug/wasmtime objdump --funcs all /tmp/demo/fib.cwasm
```

```
./target/debug/wasmtime compile -C an-encoding=y \
--emit-clif /tmp/demo/clif_on \
-o /tmp/demo/mul_on.cwasm /tmp/demo/mul.wat

./target/debug/wasmtime compile \
--emit-clif /tmp/demo/clif_off \
-o /tmp/demo/mul_off.cwasm /tmp/demo/mul.wat

./target/debug/wasmtime objdump --funcs all /tmp/demo/mul_on.cwasm
./target/debug/wasmtime objdump --funcs all /tmp/demo/mul_off.cwasm
```

Add `--target pulley64` to compile for the Pulley interpreter backend.

