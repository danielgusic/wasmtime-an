# AN-Encoding Prototype — Change Log

## Changes

- **`crates/environ/src/tunables.rs`**
  - added field `an_prototype: bool` (with false as default) as setting
  - added `AN_CONSTANT` (right now 3)

- **`crates/wasmtime/src/config.rs`**
  - added AN-encoding setting

- **`crates/wasmtime/src/engine/serialization.rs`**
  - added AN-encoding setting 

- **`tests/all/an_encoding.rs`**
  - added file, contains a few small tests

- **`tests/all/main.rs`** — added `mod an_encoding;` alongside the other test
  - inlcuded the new tests

- **`crates/cranelift/src/compiler.rs`** — `array_to_wasm_trampoline`:
  - added `AN_CONSTANT` to environ
  - when AN-encoding setting is on, args get encoded and decoded at the trampoline (only i32)

- **`crates/cranelift/src/translate/code_translator.rs`** — `I32Mul` case:
  - added `AN_CONSTANT` to environ
  - modified `Operator::I32Mul` to divide by `AN_CONSTANT` after 
  - Added `AN_CONSTANT` to the `wasmtime_environ::{...}` import

- **`crates/cli-flags/src/lib.rs`** — `CodegenOptions`:
  - added cli-flag `an-encoding-prototype=y`

## demo commands

### AN-encoding tests
cargo test -p wasmtime-cli --test all an_encoding::

### compile
./target/debug/wasmtime compile \
    -C an-encoding-prototype=y --emit-clif /tmp/demo/clif_on \
    -o /tmp/demo/mul_on.cwasm /tmp/demo/mul.wat
(add --target pulley64 for pulley)

./target/debug/wasmtime compile \
    --emit-clif /tmp/demo/clif_off \
    -o /tmp/demo/mul_off.cwasm /tmp/mul.wat

### look at assembly
./target/debug/wasmtime objdump --funcs all /tmp/demo/mul_on.cwasm
./target/debug/wasmtime objdump --funcs all /tmp/demo/mul_off.cwasm


