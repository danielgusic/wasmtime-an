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
prime recommended by the paper). The setter validates `1 ≤ A < 2³¹` (A=0 would
be impossible to decode and cause further issues [don't even think about it!]; A ≥ 2³¹ would cause problems with the sign).
Pick a large odd value (preferably prime), powers of two weaken detection.

---

## Changes

- **`crates/wasmtime/src/runtime/an_lut.rs`** *(new)*
  - Generator for AN-encoded bitwise-logical lookup tables,
inspired by Fetzer et al. 2009
  - Layout per op: 256×256 array of `i64`, indexed by `(c1 << 8) | c2` for
`c1, c2 ∈ [0, 256)`. Entries store `A · (c1 OP c2)`, 512 KB per op.
`A` validated `1 ≤ A < 2³¹`, matching the `Config::an_constant` rule.
  - Tests included
- **`crates/wasmtime/src/engine.rs`**
  - added `an_luts: Option<crate::runtime::an_lut::AnLuts>` to `EngineInner`,
populated in `Engine::new` when `tunables.an_encoding == true` via
`runtime::an_lut::generate(A)`
  - added accessor `Engine::an_lut_addr(op) -> Option<*const i64>` returning the address of
the engine's per-A `Box<[i64; 65536]>`
- **`crates/environ/src/vmoffsets.rs`**
  - VMContext layout extended with three fixed pointer slots after `type_ids`: `vmctx_an_and_table`, `vmctx_an_or_table`, `vmctx_an_xor_table`. `vmctx_dynamic_data_start` advanced past them
- **`crates/wasmtime/src/runtime.rs`**
  - included `an_lut` module
- **`crates/wasmtime/src/runtime/vm/instance.rs`**
    - added `Instance::an_{and,or,xor}_table(self) -> &mut Option<VmPtr<i64>>`
  accessors, plus `set_an_lut_pointers` invoked from `set_store` to copy
  the engine's LUT addresses into the VMContext slots when AN-encoding is
  on (slots stay `None` otherwise)
- **`crates/wasmtime/src/config.rs`** 
  - added `Config::an_encoding(bool)` and `Config::an_constant(u64)` (latter validates `1 ≤ A < 2³¹`)
- **`crates/wasmtime/src/engine/serialization.rs`** 
  - added `an_encoding` + `an_constant` checks during cwasm compatibility validation
- **`crates/cli-flags/src/lib.rs`** 
  - added `-C an-encoding=y` and `-C an-constant=N` codegen flags.
- **`crates/environ/src/tunables.rs`** 
  - added `pub const DEFAULT_AN_CONSTANT: u64 = 65521;` and the `Tunables.an_encoding: bool` (default `false`) and `Tunables.an_constant: u64` (default `DEFAULT_AN_CONSTANT`) fields
- **`crates/cranelift/src/lib.rs`**
  - added helper function `wasm_stack_value_type(isa, tunables, ty)` which widens `WasmValType::I32 → I64` when AN-encoding is on. Used by `wasm_call_signature`.
- **`crates/cranelift/src/translate/func_translator.rs`** 
  - `declare_locals` widens i32 local IR type when AN on
- **`crates/cranelift/src/translate/an_helpers.rs`**
  - implemented several helper function for bitwise operations and multiplication
- **`crates/cranelift/src/translate/code_translator.rs`**
  - implemented several operations to use AN-encoding, see *Per-op behaviour* below
- **`crates/cranelift/src/translate/translation_utils.rs`** 
  - `block_with_params` widens i32 block-param IR type when AN on.
- **`crates/cranelift/src/compiler.rs`** 
  - added respective encode/decode passes around the respective `ValRaw` boundaries to `array_to_wasm_trampoline` and `compile_wasm_to_array_trampoline` 
- **`an_encoding/`** *(new directory)* 
  - contains small wasm modules to test several things
- **`tests/all/an_encoding.rs`** *(new)* 
  - AN-encoding tests (see *Tests* below).
- **`tests/all/main.rs`** 
  - registers `mod an_encoding;`.





---

## Design

Representation inside an AN-encoded module:

| Wasm type | IR type | Holds |
|---|---|---|
| `i32` | `I64` | `A·v` with canonical `v ∈ [0, 2³²)` |
| `i64`, floats, refs, v128 | unchanged | unchanged |

`i64` support could be added in the future

**Memory model:** Encode-in-registers is used. Linear memory stays raw bytes
(wasm-native). Encoding lives in registers / locals / operand stack only.
Loads decode the address and encode the loaded value, stores decode address
*and* value. This deviates from the paper (which encodes memory at 32-bit
granularity with address remapping), the deviation is forced by WASI host
code accessing wasm linear memory directly: an encoded memory layout would
make every host-side `read()` / `write()` produce garbage. There are also other problems, like user-pointer-arithmetic, which is undetectable, since addresses are just integers is wasm.

**Function signatures:** Internal wasm function sigs are widened: every wasm
`i32` param/result becomes IR `I64`. Trampolines convert at the wasm/host
boundary so external observers (host functions, the embedder) keep seeing
raw `i32`.

**AN-encoding injection:** The operations are modified when wasm is being translated to CLIF (Cranelift intermidiate representation), since information from wasm is needed (since we can't encode wasm linear memory, we need to know which memory accesses do what [to know if we can encode them], which would be lost in later stages)

**Canonical invariant:** After every operation, encoded i32 values are
brought back to `A·v with v ∈ [0, 2³²)`. This is what lets compares,
addresses, and host-call args work directly on the encoded form without
needing per-use decode.

### Per-op behaviour

| Op | Strategy |
|---|---|
| `i32.const k` | emit `iconst.i64 (A·k)` |
| `i32.add` | `iadd` then canonicalize via overflow-check: `sum >= A·2³² ? sum - A·2³² : sum` |
| `i32.sub` | `isub` then canonicalize via underflow-check: `diff < 0 ? diff + A·2³² : diff` |
| `i32.mul` | `(P_hi, P_lo) = (umulhi, imul)(A·n, A·m) → udiv_u128_by_u64_const(·, A) → umod_u128_by_u64_const(·, A·2³²)`. See *i32.mul* note below |
| `i32.div_u` | `(arg1 udiv arg2) · A` (one A naturally cancels) |
| `i32.rem_u` | unchanged: `A·n urem A·m = A·(n urem m)`  |
| `i32.div_s`, `i32.rem_s` | not yet implemented |
| `i32.eqz` | `icmp_imm Equal arg 0` produces an i8 boolean, then `select(bool, A, 0)` to encode as `0`/`A` |
| `i32.lt_u`, `le_u`, `gt_u`, `ge_u`, `eq`, `ne` | compare encoded operands directly (A preserves order + zero), then `select(bool, A, 0)` to encode the boolean result |
| `i32.lt_s`, `le_s`, `gt_s`, `ge_s` | remap each operand to `c' = (c + A·2³¹) mod (A·2³²)`, then unsigned compare |
| `i32.and`, `i32.or`, `i32.xor` | tabulated on functional 8-bit chunks via `emit_an_bitwise_i32` (paper-style). One `udiv` per operand decodes; four `(c1<<8)|c2` indexes load `A·(c1 OP c2)` from a 256×256 `i64` table, then `acc += entry << (8·i)` recombines to `A·(n OP m)`. Tables live on the `Engine` (per-A, generated by `wasmtime-an-lut`); their address is loaded from a fixed `VMContext` slot at op-site (`load.i64 [vmctx + offset]`), so the same machine code is portable across processes. |
| `i32.not` | wasm has no native `i32.not`; written as `i32.const -1; i32.xor` and follows the `i32.xor` LUT path. |
| `i32.load{,8_u,16_u}` | for memory32 (i32 indices): decode addr (÷A → trunc.i32) → wasm load (raw) → `uextend.i64` → ·A. For memory64 the popped i64 address is raw and passes through (not yet supported), the loaded value is still encoded if the result type is i32. |
| `i32.store{,8,16}` | for memory32: decode addr, decode value (÷A → trunc.i32); wasm store raw. For memory64: address passes through raw (not yet supported), value still decoded since wasm-level type is i32. |
| `local.{get,set,tee}`, `global.{get,set}` (i32) | type widened to I64 by the sig/locals widening |
| `br_if` / `if` / `select` cond | unchanged |
| host-import call (wasm → host) | decode i32 args, encode i32 returns at the `wasm_to_array` trampoline |
| host → wasm entry call | encode i32 args, decode i32 returns at the `array_to_wasm` trampoline |



### `i32.mul` note

To implement `i32.mul` so that it stays encoded, the division uses algorithm 4 proposed in the paper "Improved Division by Invariant Integers", Möller & Granlund, 2010.
High level overview (see `crates/cranelift/src/translate/an_helpers.rs` for more details):
1. Calculate the raw product P  = (A·n) · (A·m) = A²·n·m
2. Calculate the quotient Q  = P / A = A·n·m
3. Canonicalize the result R = Q mod (A·2³²) = A·(n·m mod 2³²)

For this, several helper functions have been implemented.


### (Yet) Unsolved Problems


| Problem | Description | Idea how to solve |
|---|---|---|
| linear memory | wasm uses it for a lot of things, especially interaction with other things at runtime (e.g. wasi syscalls) | have an encoded and unencoded version at the same time? |
| i64 support | encoded version of i64 values would need 128 bit (and even more with operations like mul), but 128 bit support is non-existent | enormous amounts of i64 concatenation hacks |



### Future work

Signed div/rem, shifts (paper's `power-of-two` table → mul/div), 64-bit
wasm ops, floats, SIMD, multi-memory, GC types, in-memory AN encoding,
codeword-validity checks (`mod A == 0` assertions at trampoline
boundaries, opt-in via tunable).



---

## Tests

```cargo test -p wasmtime-cli --test all an_encoding::``` 
group with AN off and on:

| Test | Coverage |
|---|---|
| `mul_{without_an,with_an}` | `i32.mul` end-to-end. Tests run on the native backend (default since the Pulley/Native split was dropped — both behave identically under the AN paths). |
| `fib_{without_an,with_an}` | `an_encoding/fib.wat` end-to-end via WASI preview1 (`MemoryInputPipe` / `MemoryOutputPipe`) |
| `ops_{without_an,with_an}` | one wat module exporting one function per touched operator: add, sub, mul, divu, remu, addconst, lt_u, ge_u, gt_u, eq, ne, eqz, lt_s/le_s/gt_s/ge_s  |
| `ops_with_an_custom_constants` | re-runs the `ops_*` assertions with several non-default values of `A` (1, 7, 1009, 16 777 213) to verify the codegen reads `A` from `Tunables` rather than baking the default in |

Both AN modes are required to produce identical results.

---

## Demo commands

### Run fib

```
./target/debug/wasmtime run --dir . -C an-encoding=y an_encoding/fib.wat
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
