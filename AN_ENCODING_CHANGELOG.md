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
| `Config` (Rust API) | `config.an_encoding(true)` (and optionally `config.an_constant(a)` / `config.an_load_validity_check(true)`) |
| CLI | `wasmtime ... -C an-encoding=y [-C an-constant=N] [-C an-load-validity-check=y] ...` |
| `Tunables` fields | `an_encoding: bool`, `an_constant: u64`, `an_load_validity_check: bool` |
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



### Runtime - `crates/wasmtime`

- **`runtime/an_lut.rs`** *(new)* 
  - generator for the per-`A` bitwise-logical
  lookup tables (`and`/`or`/`xor`). Each is a 256×256 `i32` array indexed by
  `(c1 << 8) | c2`, holding `A · (c1 OP c2)` (256 KiB per op).
- **`engine.rs`**
  - `EngineInner` owns the generated `AnLuts` (built in `Engine::new` when AN is on)
  - `Engine::an_lut_addr(op)` exposes each table's address so the same JIT code is portable across processes
- **`runtime.rs`**
  - registers the `an_lut` module
- **`runtime/vm/instance.rs`**
  - per-instance AN state: LUT pointer slots copied into the `VMContext` on `set_store`, and an `an_enc_shadows` map owning one encoded shadow `Box<[u8]>` per defined memory
  - the encode / cross-check / range-re-encode / grow routines that keep each shadow in lockstep with raw memory (used by the allocator and the libcalls)
- **`runtime/vm/instance/allocator.rs`**
  - after memory initialization, mirrors data-segment / CoW content into each shadow before wasm starts
- **`runtime/memory.rs`**
  - `#[doc(hidden)]` test-only `Memory::an_shadow_data_mut_for_test` accessor that hands out the encoded shadow as a mutable slice, so the fault-injection tests can tamper the shadow directly
- **`runtime/vm/libcalls.rs`**
  - `an_check_host_boundary` / `an_resync_host_boundary` libcalls (cross-check before a host call, re-encode after)
  - shadow updates appended to the `memory.grow/copy/fill/init` libcalls
- **`compile.rs`**
  - `validate_an_encoding_constraints` runs after translation and, when AN is on, refuses unsupported features: imported and shared memories, all floating-point types/operations (checked at function signatures, globals, locals, and operators), and atomic memory operators (threads proposal)
  - memory64 and `i32 ↔ i64` conversion ops are allowed with a one `log::warn!`
- **`config.rs`**
  - `Config::an_encoding(bool)`, `Config::an_constant(u64)` (validates `1 ≤ A < 2²³`), `Config::an_load_validity_check(bool)`
  - `#[doc(hidden)]` test-only knobs `Config::an_inject_codeword_fault(u64)` (trampoline boundaries) and `Config::an_inject_conversion_fault(u64)` (conversion-op decode sites)
- **`engine/serialization.rs`**
  - includes the AN tunables in cwasm compatibility validation
- **`runtime/externals/global.rs`**
  - host-boundary global encode/decode: `Global::get` decodes the stored `A·v`, `Global::set` (and instantiation's `set_unchecked`) encodes. `an_constant_for_i32` gates this to wasm-module i32 globals (`Instance`/`Host`), excluding the component flag globals
- **`runtime/trampoline/global.rs`**
  - `generate_global_export` encodes the initial value of a host-created (`Global::new`) i32 global
- **`runtime/vm/vmcontext.rs`**
  - `VMGlobalDefinition::{from,to}_val_raw` encode/decode the i32 `ValRaw` ↔ storage conversion

### Environment - `crates/environ`

- **`vmoffsets.rs`**
  - `VMContext` gains three fixed LUT pointer slots (`and`/`or`/`xor`) after `type_ids`
  - per-defined-memory `defined_memories_enc_bases` array (shadow base pointers), with accessors
- **`builtin.rs`**
  - declares the `an_check_host_boundary` / `an_resync_host_boundary` builtins (`-> bool`; a falsy return becomes a trap at the trampoline)
- **`trap_encoding.rs`**
  - new traps `Trap::AnMemoryMismatch` (`48`) and `Trap::AnCodewordInvalid` (`49`)
  - the `crates/c-api/src/trap.rs` const-asserts are updated to match
- **`tunables.rs`**
  - `DEFAULT_AN_CONSTANT = 65521` and `ENC_MEM_GROWTH_FACTOR = 2`
  - the `an_encoding` / `an_constant` / `an_load_validity_check` / `an_inject_codeword_fault` / `an_inject_conversion_fault` fields

### Cranelift - `crates/cranelift`

- **`lib.rs`**
  - `wasm_stack_value_type` widens wasm `i32 → I64` under AN (used by `wasm_call_signature`)
  - `TRAP_AN_MEMORY_MISMATCH` / `TRAP_AN_CODEWORD_INVALID` trap codes
- **`translate/an_helpers.rs`**
  - all AN codegen helpers: value encode/decode, the bitwise-LUT path, multiplication, shifts/rotates, the shadow-store read-modify-write helpers, the per-load validity check, and the boundary / conversion codeword checks
- **`translate/code_translator.rs`**
  - implements AN-encoding for the supported operators (see *Per-op behaviour*)
  - mirrors `i32.store{,8,16}` into the shadow
  - decodes/re-encodes the i32 operands around the memory- and table-index builtins (`memory.*`, `table.*`, `call_indirect`)
  - `global.get`/`global.set` no longer transform the value: storage is encoded, so loads/stores are pass-through
- **`func_environ.rs`**
  - `make_global` widens i32 global storage to `I64` under AN; `translate_global_get` emits `iconst.i64 (A·v)` for constant-folded immutable i32 globals
- **`translate/func_translator.rs`**
  - widens the i32 local IR type under AN
- **`translate/translation_utils.rs`**
  - widens the i32 block-param IR type under AN
- **`translate/mod.rs`**
  - re-exports `emit_an_codeword_validity_check` from `an_helpers` for the trampoline codegen
- **`compiler.rs`**
  - the wasm/host trampolines (`array_to_wasm_trampoline`, `compile_wasm_to_array_trampoline`) encode/decode i32 at the boundary
  - emit the boundary codeword check at each decode site, and bracket host calls with the cross-check / resync libcalls
- **`compiler/component.rs`**
  - the component hostcall trampoline (`translate_hostcall`) gets the same i32 encode/decode + codeword check + cross-check/resync treatment for canon-lowered host imports

### CLI & tests

- **`crates/cli-flags/src/lib.rs`**
  - `-C an-encoding=y`, `-C an-constant=N`, `-C an-load-validity-check=y`
- **`an_encoding/`** *(new directory)*
  - small wat/wasm modules used by the tests
- **`tests/all/an_encoding.rs`** *(new)*
  - the AN-encoding test suite (see *Tests*)
- **`tests/all/main.rs`**
  - registers `mod an_encoding;`

---

## Design

Representation inside an AN-encoded module:

| Wasm type | IR type | Holds |
|---|---|---|
| `i32` | `I64` | `A·v` with canonical `v ∈ [0, 2³²)` |
| `i64`, refs, v128 | unchanged | passed through, not encoded |

`i64` support could be added in the future. Floating-point types (`f32`/`f64`)
are refused outright when AN-encoding is on (see *Refused / unsolved features*).

**Memory model:** two linear memories, one mirroring the other but encoded

Everything done with the linear memory is unchanged, but mirrored and encoded in a shadow as well, when AN-encoding is turned on, so there are two memories resulting in a 3x memory increase.

At every wasm->host call boundary the wasm-to-array trampoline walks every defined memory and asserts `[slot] % A == 0 && [slot] / A == u32_le(raw[4i..4i+4])` for every slot. Any mismatch traps as `Trap::AnMemoryMismatch`. 
Immediately after the host returns, the trampoline re-encodes raw bytes into the shadow so that host-side writes (e.g. WASI `fd_read`) get reflected before wasm resumes.



Some design decision regarding the memory:
- The additional shadow has 2x the size of the regular memory
- Checking for correctness is only done at wasm<->host boundaries for now
  - At those boundaries, the whole linear memory is checked
- The shadow is always updated according to the regular memory at the same time (directly after)
- Unaligned/subword accesses use RMW like paper
- Shared/atomic and imported memories are refused when AN-encoding is on; multi-memory is supported
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

### Supported features

| Feature | Status under AN-encoding |
|---|---|
| `i32` | everything based on i32 should work, excepting conversions to not supported types and atomics, except i64 (see *Per-op behaviour*) |
| linear memory (incl. multi-memory, bulk-memory, `memory.grow`) | encoded shadow, cross-checked at host boundaries |
| tables, `call_indirect` | i32 index/length operands decoded around the builtin |
| `i64` | allowed, but outside of encoding, warning emitted |
| imported / shared (atomic) memories, atomic operators | **refused** at compile time |

**Note**: See *Refused / unsolved features below*

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
| `i32.load{,8_u,16_u,8_s,16_s}` | for memory32 (i32 indices): decode addr (÷A → trunc.i32) → wasm load (raw) → `uextend.i64` → ·A. Loads pull from the raw buffer; the cross-check at the next host-call boundary catches any divergence in the shadow. For memory64 the popped i64 address is raw and passes through, the loaded value is still encoded if the result type is i32. When `Tunables.an_load_validity_check` is on, an extra inline assertion (`enc_slot == A * u32_le(raw_slot)`) fires per touched shadow slot BEFORE the raw load; mismatch → immediate `AnMemoryMismatch` trap. |
| `i32.store` (4-byte) | decode addr, decode value (÷A → trunc.i32); wasm store raw. **Plus** AN-encoded mirror: runtime branch on `effective_addr & 3`. Aligned path (`byte_pos == 0`) does a single `store.i64 [enc_base + 2*effective_addr]` of the encoded operand `A*v`. Unaligned path decomposes into four byte-RMWs at consecutive byte addresses; each helper computes its own slot index so cross-slot transitions fall out automatically. |
| `i32.store8` | decode addr, decode value; wasm store raw byte. **Plus** single byte-RMW on the shadow slot containing the target byte. `i32.store8` always fits in one slot. |
| `i32.store16` | decode addr, decode value; wasm store raw half. **Plus** two byte-RMWs at `effective_addr` and `effective_addr + 1`. Covers in-slot (`byte_pos in 0..=2`) and cross-slot (`byte_pos == 3`) cases uniformly because each byte-RMW computes its own slot index. |
| `local.{get,set,tee}` (i32) | type widened to I64 by the sig/locals widening |
| `global.get` (i32) | i32 globals are stored encoded, so no per-access tranform is needed for the guest. Their `VMGlobalDefinition` storage type is widened to `I64` in `make_global` (the slot is 16 bytes, so there is room). Imports, defined globals, and constant-folded immutable globals all load the encoded form (constant-folded ones emit `iconst.i64 (A·v)` directly). Decoding happens only at external boundaries |
| `global.set` (i32) | the operand is already the canonical encoded `A·v` (`I64`), so no change is needed. Non-i32 globals pass through unchanged. Encoding/decoding happens only at external boundaries |
| `i32.extend8_s` / `i32.extend16_s` | stays inside the encoding. Decode (`udiv → ireduce.i32`, no codeword check because of structural invariant, matches `clz`/`ctz`/`popcnt`), sign-extend the low byte/half-word to i32, re-encode via `emit_an_encode_raw_i32` (`uextend.i64 → · A`). |
| `i32.wrap_i64` | raw i64 → encoded i32. Take low 32 bits (`ireduce.i32`), re-encode. Wasm-spec: no trap. Input is *not* a codeword (raw i64), so no codeword check. Compile emits a one-shot per-module warning ([Conversion warning](#conversion-warning)). |
| `i64.extend_i32_s` / `i64.extend_i32_u` | encoded i32 → raw i64. Boundary codeword check via `emit_an_conversion_decode_i32` (optionally bumps by `an_inject_conversion_fault` first), then `urem` + `trapnz` against `Trap::AnCodewordInvalid`, then decode `udiv A → ireduce.i32`, then `sextend`/`uextend` to `I64`. Output leaves the AN encoding; warning emitted at compile time. |
| `br_if` / `if` / `select` cond | unchanged |
| host-import call (wasm → host) | decode i32 args, encode i32 returns at the `wasm_to_array` trampoline. Additionally: emit `an_check_host_boundary` libcall **before** the host call to cross-check every defined memory's encoded shadow against raw bytes (any mismatch raises `Trap::AnMemoryMismatch`), and emit `an_resync_host_boundary` libcall **after** the host returns to re-encode raw bytes into the shadow (so direct host writes via `Memory::data_mut` or WASI surface in the shadow before wasm resumes). **Boundary codeword check** is emitted on every encoded i32 arg before the `udiv` decode: `val % A != 0 → Trap::AnCodewordInvalid`. |
| host → wasm entry call | encode i32 args, decode i32 returns at the `array_to_wasm` trampoline. **Boundary codeword check** is emitted on every encoded i32 result before the `udiv` decode. |




### `i32.mul` note

To implement `i32.mul` so that it stays encoded, the division uses algorithm 4 proposed in the paper "Improved Division by Invariant Integers", Möller & Granlund, 2010.
High level overview (see `crates/cranelift/src/translate/an_helpers.rs` for more details):
1. Calculate the raw product P  = (A·n) · (A·m) = A²·n·m
2. Calculate the quotient Q  = P / A = A·n·m
3. Canonicalize the result R = Q mod (A·2³²) = A·(n·m mod 2³²)

For this, several helper functions have been implemented.


### Refused / unsolved / WIP features


| Feature | Why it's refused / unsolved | Idea how to solve |
|---|---|---|
| floating point | f32/f64 types and every float operator are refused at compile time | -
| i64 support | an encoded i64 would need 128 bit (and even more with operations like mul), but 128 bit support is non-existent | enormous amounts of i64 concatenation hacks |
| shared/atomic memory, imported memory | refused at compile time | shared memories need atomic-safe shadow stores, imported memories need cross-instance shadow ownership, atomic ops need read-modify-write shadow paths that respect threads-proposal ordering |
| SIMD | not implemented
| GC types | not implemented
| wmemcheck | not implemented, should break I think


### Conversion warning

When `tunables.an_encoding == true` and the module contains an `i32 ↔ i64`
conversion op (e.g. `i32.wrap_i64`, `i64.extend_i32_s/u`),
`validate_an_encoding_constraints` walks every function body once and emits a
single `log::warn!` per module: i32 values crossing these ops leave the AN
encoding and the resulting i64 operands are not AN-protected.
The implementation lives in
`crates/wasmtime/src/compile.rs::is_i32_i64_conversion_op`.

### Validity checks

Codeword-validity is checked at every encoding boundary (e.g. `val % A == 0`). This includes leaving the encoding by coverting to `i64` and trampoline boundaries, which are
on the wasm/host and host/wasm boundaries. Both for the core-wasm
trampolines (`compiler.rs`) and the component-model `translate_hostcall` path
(`compiler/component.rs`). See *New traps* below.

Memory validity checks are checked in the same place. At every boundary, the whole memory is checked (could be optimized in the future).

Always-on when `Tunables.an_encoding` is set.

### New traps

`Trap::AnMemoryMismatch` (variant `48`) is raised by
`an_check_host_boundary` libcall when the encoded shadow of any defined
linear memory disagrees with raw bytes. 

When `Tunables.an_load_validity_check` is on the same trap is *also* raised
inline at the load site by `emit_an_load_validity_check`. The trap code is
the same; the difference is the source location,  `an_check_host_boundary`
fires at a host-call boundary, whereas the load-side check fires at the
exact wasm op that observed the divergence.

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





---

## Tests

```cargo test -p wasmtime-cli --test all an_encoding::``` 
group with AN off and on:

| Test | Coverage |
|---|---|
| `mul_{without_an,with_an}` | `i32.mul` end-to-end on the native backend |
| `fib_{without_an,with_an}` | `an_encoding/fib.wat` end-to-end via WASI preview1 (`MemoryInputPipe` / `MemoryOutputPipe`) |
| `fib_with_an_and_load_validity_check` | same fib run with `an_load_validity_check(true)` on top of AN |
| `ops_{without_an,with_an}` | one wat module exporting one function per touched operator: add, sub, mul, divu, remu, divs, rems, addconst, lt_u, ge_u, gt_u, eq, ne, eqz, lt_s/le_s/gt_s/ge_s, and/or/xor/not/mask_merge, shl/shr_u/shr_s/rotl/rotr, clz/ctz/popcnt, max_u, loop_count, digits, memory load/store, mutable i32 global (g_get/g_set/g_inc) plus negative immutable initializer. Shifts/rotations cover 12 value patterns × 14 shift counts (including wraparound > 32). Includes trap assertions for `div_s` (`/0`, `INT_MIN/-1`) and `rem_s` (`/0`, `INT_MIN%-1 → 0`). |
| `ops_with_an_custom_constants` | re-runs the `ops_*` assertions with several non-default values of `A` (1, 7, 1009, 2²³ − 1) to verify the codegen reads `A` from `Tunables` rather than baking the default in |
| `global_boundary_{without,with}_an` / `global_import_{without,with}_an` / `global_boundary_various_an_constants` | host-boundary global encode/decode. `global_boundary_*` exports a mutable and an immutable i32 global directly and cross-checks the host view (`Global::get`/`set`) against the guest view (`global.get`/`set`) over a value matrix (incl. negatives, `i32::MIN/MAX`); the host always sees raw values while storage stays encoded. `global_import_*` imports a host-created (`Global::new`) i32 global into the module, exercising the `VMGlobalKind::Host` storage path (host init + `set`/`get` + guest mutation round-trip). `_various` re-runs both under `A ∈ {1, 7, 1009, 2²³ − 1}`. AN-off counterparts confirm identical behavior. |
| `refuse_float_{param,result,local,global,op}_under_an` | a float in a function signature, global, local, or operator stream must fail compilation under AN with a "floating-point" message |
| `refuse_imported_memory_under_an` / `refuse_shared_memory_under_an` | each compiles a wat module that violates the supported-feature matrix and asserts the error mentions AN-encoding |
| `multi_memory_compiles_under_an` / `multi_memory_stores_keep_shadows_consistent` / `multi_memory_tamper_{mem0,mem1}_traps` / `multi_memory_clean_run_passes` | multi-memory module with two defined memories: stores route to each via `memarg.memory`, the host-boundary cross-check visits both shadows, and tampering either memory's raw bytes raises `AnMemoryMismatch` |
| `load_validity_check_default_off` / `load_validity_check_clean_run_passes` / `load_validity_check_traps_on_{raw_tamper,load8u,load16u_cross_slot}` / `load_validity_check_traps_unaligned_i32_load` / `load_validity_check_various_an_constants` | opt-in per-load check: with `an_load_validity_check(true)`, tampering raw bytes via `Memory::data_mut` between instantiation and a wasm load makes the load raise `AnMemoryMismatch` immediately. Covers `i32.load`/`load8_u`/`load16_u`, aligned + unaligned + cross-slot positions, and several A values. The default-off counterpart confirms the check is gated correctly. |
| `table_{size,grow,fill,copy,init}_under_an` / `call_indirect_under_an` / `table_ops_match_without_an` | a wat with a 4-element funcref table exercises each table op under AN-on and confirms behavior matches the AN-off baseline. Without the per-operand decode, encoded i64 operands flowing into `cast_index_to_i64` panic in cranelift. `call_indirect` covers the vtable dispatch case (the hot path for closures / virtual calls in real wasm). |
| `component_an::component_compiles_{without,with}_an` / `component_an::component_with_an_various_constants` | component-model integration: a component wraps a core module that does an `i32.store` and then calls a host import via canon-lower. The AN cross-check + resync libcalls fire from the component hostcall trampoline using the core caller's vmctx. The "various constants" case re-runs across `A ∈ {1, 7, 1009, 65521, 2^23 − 1}` to confirm the libcalls read `A` from the engine tunables. |
| `refuse_atomic_{load,store,rmw_add,rmw_cmpxchg,fence}_under_an` / `refuse_memory_atomic_{notify,wait32}_under_an` | each compiles a wat module exercising a representative threads-proposal atomic operator and asserts compilation fails with "AN-encoding" in the message |
| `memory64_with_an_is_allowed_with_warning` | memory64 + AN compiles (warning-only) |
| `instantiate_data_segment_under_an` | smoke test: AN-encoding shadow init does not panic when a data segment is present at instantiation |
| `fault_inject_flip_in_raw_traps` / `fault_inject_flip_in_shadow_traps` | flip a bit in raw memory (`Memory::data_mut`) resp. in the encoded shadow (`an_shadow_data_mut_for_test`) after instantiation; the next host-call boundary cross-check raises `Trap::AnMemoryMismatch` |
| `fault_inject_various_an_constants` | the fault-injection trap fires for every legal `A` (1, 7, 1009, 65521, 2²³ − 1) |
| `fault_inject_clean_run_passes` | sanity counterpart without tampering the host-call boundary does not trap |
| `unaligned_i32_store_every_offset` | `i32.store` at every byte offset 0..7 with 4-byte value; verifies raw bytes plus the host-boundary cross-check |
| `cross_slot_i32_store16_every_offset` | `i32.store16` at every byte offset 0..7, exercising in-slot (`byte_pos in 0..=2`) and cross-slot (`byte_pos == 3`) paths |
| `unaligned_store_then_aligned_store_same_slot` | aligned `i32.store` overwriting a slot previously touched by an unaligned byte-RMW path, confirms the slot stays a valid `A * u32` codeword |
| `bulk_wat_compiles_{without,with}_an` | smoke test: a module exercising `memory.fill/copy/init/grow/size` plus `i32.store8/load` compiles cleanly under both AN modes |
| `bulk_memory_fill_keeps_shadow_consistent` | `memory.fill` over aligned + unaligned + cross-slot ranges; cross-check passes |
| `bulk_memory_copy_keeps_shadow_consistent` | non-overlapping and overlapping `memory.copy`; verifies `memmove`-style overlap handling |
| `active_data_segment_keeps_shadow_consistent` / `passive_memory_init_keeps_shadow_consistent` | active data segment mirrored into the shadow at instantiation, and `memory.init` of a passive segment kept consistent |
| `bulk_memory_grow_keeps_shadow_consistent` | `memory.grow` preserves a pre-grow sentinel byte and the freshly grown pages encode as zero |
| `bulk_memory_with_various_an_constants` | bulk-op + cross-check loop across `A` ∈ {1, 7, 1009, 65521, 2^23−1} |
| `codeword_check::codeword_check_clean_wasm_to_host_args` / `codeword_check_clean_wasm_to_host_multi_args` / `codeword_check_clean_wasm_to_host_no_i32_params` / `codeword_check_clean_host_to_wasm_returns` / `codeword_check_clean_repeated_host_calls` / `codeword_check_clean_various_an_constants` / `codeword_check_no_trap_when_an_off` | boundary codeword check positive coverage. Every wasm/host trampoline shape (one/many i32 args, no-i32, return-only, many calls, every legal `A`) completes without false-positive. AN-off counterpart confirms the check is gated correctly. |
| `codeword_check::codeword_check_traps_wasm_to_host_args_with_injection` / `codeword_check_traps_host_to_wasm_returns_with_injection` / `codeword_check_traps_various_an_constants` | boundary codeword check negative coverage. With `Config::an_inject_codeword_fault(1)` set, the trampoline bumps the first encoded i32 arg/result by 1 before the modulo check fires; the check is guaranteed to trap as `Trap::AnCodewordInvalid` for any `A > 1`. Covers both directions (wasm→host args, host→wasm returns) and several `A` values. |
| `component_codeword::component_i32_arg_passthrough_without_an` / `component_i32_arg_passthrough_with_an` / `component_i32_multi_arg_with_an` / `component_i32_various_an_constants` / `component_codeword_check_traps_with_injection` | components with `u32`-typed imports round-trip correctly under AN (single arg, multi arg, every legal `A`). AN-off baseline confirms the wat is well-formed. The fault-inject negative case confirms the boundary codeword check fires on the component hostcall trampoline like the core path. |
| `conversions::conversions_without_an` / `conversions_refused_under_an` | the float-containing `an_encoding/conversions.wat` runs end-to-end as an AN-off baseline (incl. wasm-spec trap behaviour of `i32.trunc_f*_s/u`: NaN → `BadConversionToInteger`; ±∞ / out-of-range / negative-into-unsigned → `IntegerOverflow`); under AN it must be refused with a "floating-point" message |
| `int_conversions::int_conversions_{without,with}_an` / `int_conversions_with_various_an_constants` | the float-free `an_encoding/int_conversions.wat` (`i32.extend8_s/16_s`, `i32.wrap_i64`, `i64.extend_i32_s/u`) produces identical results AN-on and AN-off. Edge cases: sign-extend bit boundaries (0x7F/0x80/0xFF), wrap from `i64::MAX/MIN` and `0x1_0000_0000`, `extend_i32_u` of negatives. `_various` re-runs for `A ∈ {1, 7, 1009, 65521, 2^23 − 1}`. |
| `int_conversions::int_conversions_codeword_check_traps_with_injection` / `int_conversions_codeword_check_traps_various_an_constants` / `int_conversions_no_codeword_trap_without_injection` | conversion boundary-codeword check coverage on `i64.extend_i32_s/u`. `Config::an_inject_conversion_fault(1)` bumps the encoded i32 by 1 at the decode site (harness funcs source their i32 from `i32.const`, so the conversion site is the first decode boundary), guaranteeing a `Trap::AnCodewordInvalid` for `A > 1`. `_various` covers `A ∈ {7, 1009, 65521, 2^23 − 1}`. The no-injection counterpart confirms no false-positive at any legal A. |

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

