use wasmtime::{Config, Engine, Linker, Module, Store};
use wasmtime_environ::TripleExt;

const MUL_WAT: &str = r#"
    (module
        (func (export "mul") (param i32 i32) (result i32)
            local.get 0
            local.get 1
            i32.mul)
    )
"#;

#[derive(Copy, Clone, Debug)]
enum Backend {
    Native,
    Pulley,
}

fn make_config(backend: Backend, an_enabled: bool) -> Config {
    let mut config = Config::new();
    if let Backend::Pulley = backend {
        let triple = target_lexicon::Triple::pulley_host().to_string();
        config.target(&triple).unwrap();
    }
    config.an_encoding(an_enabled);
    config
}

fn run_mul(backend: Backend, an_enabled: bool, a: i32, b: i32) -> wasmtime::Result<i32> {
    let engine = Engine::new(&make_config(backend, an_enabled))?;
    let module = Module::new(&engine, MUL_WAT)?;
    let mut store = Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
    let mul = instance.get_typed_func::<(i32, i32), i32>(&mut store, "mul")?;
    mul.call(&mut store, (a, b))
}

fn check(backend: Backend, an_enabled: bool) -> wasmtime::Result<()> {
    assert_eq!(run_mul(backend, an_enabled, 7, 6)?, 42);
    assert_eq!(run_mul(backend, an_enabled, 0, 123)?, 0);
    assert_eq!(run_mul(backend, an_enabled, -3, 4)?, -12);
    Ok(())
}

#[test]
fn mul_without_an() -> wasmtime::Result<()> {
    check(Backend::Pulley, false)
}

#[test]
fn mul_with_an() -> wasmtime::Result<()> {
    check(Backend::Pulley, true)
}

#[test]
fn mul_without_an_native() -> wasmtime::Result<()> {
    check(Backend::Native, false)
}

#[test]
fn mul_with_an_native() -> wasmtime::Result<()> {
    check(Backend::Native, true)
}

const FIB_WAT: &str = include_str!("../../an_encoding/fib.wat");

fn run_fib(an_enabled: bool, n: u32) -> wasmtime::Result<String> {
    let mut config = Config::new();
    config.an_encoding(an_enabled);
    let engine = Engine::new(&config)?;
    let module = Module::new(&engine, FIB_WAT)?;
    let mut linker: Linker<wasmtime_wasi::p1::WasiP1Ctx> = Linker::new(&engine);
    wasmtime_wasi::p1::add_to_linker_sync(&mut linker, |t| t)?;

    let stdin = wasmtime_wasi::p2::pipe::MemoryInputPipe::new(format!("{n}\n"));
    let stdout = wasmtime_wasi::p2::pipe::MemoryOutputPipe::new(64);
    let ctx = wasmtime_wasi::WasiCtxBuilder::new()
        .stdin(stdin)
        .stdout(stdout.clone())
        .build_p1();
    let mut store = Store::new(&engine, ctx);

    let instance = linker.instantiate(&mut store, &module)?;
    let start = instance.get_typed_func::<(), ()>(&mut store, "_start")?;
    start.call(&mut store, ())?;
    drop(store);

    let bytes = stdout.contents();
    Ok(String::from_utf8_lossy(&bytes).trim().to_string())
}

fn fib_check(an_enabled: bool) -> wasmtime::Result<()> {
    assert_eq!(run_fib(an_enabled, 0)?, "0");
    assert_eq!(run_fib(an_enabled, 1)?, "1");
    assert_eq!(run_fib(an_enabled, 2)?, "1");
    assert_eq!(run_fib(an_enabled, 5)?, "5");
    assert_eq!(run_fib(an_enabled, 10)?, "55");
    assert_eq!(run_fib(an_enabled, 20)?, "6765");
    assert_eq!(run_fib(an_enabled, 30)?, "832040");
    Ok(())
}

#[test]
fn fib_without_an() -> wasmtime::Result<()> {
    fib_check(false)
}

#[test]
fn fib_with_an() -> wasmtime::Result<()> {
    fib_check(true)
}

// Per-operator regression suite. One wat module covers every i32 operator that
// the AN-encoding prototype touches; each `ops_*` test runs the same module
// with AN off and on and asserts the same expected results from both. The wat
// is intentionally simple so failures point straight at a specific operator.
const OPS_WAT: &str = r#"
    (module
        (memory (export "memory") 1)

        ;; arithmetic
        (func (export "add") (param i32 i32) (result i32)
            local.get 0 local.get 1 i32.add)
        (func (export "sub") (param i32 i32) (result i32)
            local.get 0 local.get 1 i32.sub)
        (func (export "mul") (param i32 i32) (result i32)
            local.get 0 local.get 1 i32.mul)
        (func (export "divu") (param i32 i32) (result i32)
            local.get 0 local.get 1 i32.div_u)
        (func (export "remu") (param i32 i32) (result i32)
            local.get 0 local.get 1 i32.rem_u)

        ;; const-mixing (exercises i32.const encoding alongside add)
        (func (export "addconst") (param i32) (result i32)
            local.get 0 i32.const 100 i32.add)

        ;; comparisons + eqz
        (func (export "lt_u") (param i32 i32) (result i32)
            local.get 0 local.get 1 i32.lt_u)
        (func (export "ge_u") (param i32 i32) (result i32)
            local.get 0 local.get 1 i32.ge_u)
        (func (export "gt_u") (param i32 i32) (result i32)
            local.get 0 local.get 1 i32.gt_u)
        (func (export "eq") (param i32 i32) (result i32)
            local.get 0 local.get 1 i32.eq)
        (func (export "ne") (param i32 i32) (result i32)
            local.get 0 local.get 1 i32.ne)
        (func (export "eqz") (param i32) (result i32)
            local.get 0 i32.eqz)

        ;; if/else (exercises br_if-on-encoded-cond + block params)
        (func (export "max_u") (param i32 i32) (result i32)
            (if (result i32) (i32.gt_u (local.get 0) (local.get 1))
                (then local.get 0)
                (else local.get 1)))

        ;; loop with br_if (encoded counter, accumulator, const)
        (func (export "loop_count") (param i32) (result i32)
            (local $i i32)
            (local $sum i32)
            (block $break
                (loop $l
                    (br_if $break (i32.ge_u (local.get $i) (local.get 0)))
                    (local.set $sum (i32.add (local.get $sum) (local.get $i)))
                    (local.set $i (i32.add (local.get $i) (i32.const 1)))
                    (br $l)))
            (local.get $sum))

        ;; loop using div_u (from fib's $format)
        (func (export "digits") (param i32) (result i32)
            (local $n i32)
            (local $c i32)
            (local.set $n (local.get 0))
            (block $break
                (loop $l
                    (br_if $break (i32.eqz (local.get $n)))
                    (local.set $n (i32.div_u (local.get $n) (i32.const 10)))
                    (local.set $c (i32.add (local.get $c) (i32.const 1)))
                    (br $l)))
            (local.get $c))

        ;; memory: i32.store / i32.load round-trip
        (func (export "store_load_i32") (param i32 i32) (result i32)
            local.get 0 local.get 1 i32.store
            local.get 0 i32.load)

        ;; memory: i32.store8 / i32.load8_u round-trip
        (func (export "store_load_byte") (param i32 i32) (result i32)
            local.get 0 local.get 1 i32.store8
            local.get 0 i32.load8_u)

        ;; mixed: write 0..n into memory[100..], sum back via load8_u
        (func (export "sum_bytes") (param i32) (result i32)
            (local $i i32)
            (local $sum i32)
            (block $w
                (loop $lw
                    (br_if $w (i32.ge_u (local.get $i) (local.get 0)))
                    (i32.store8 (i32.add (i32.const 100) (local.get $i)) (local.get $i))
                    (local.set $i (i32.add (local.get $i) (i32.const 1)))
                    (br $lw)))
            (local.set $i (i32.const 0))
            (block $r
                (loop $lr
                    (br_if $r (i32.ge_u (local.get $i) (local.get 0)))
                    (local.set $sum
                        (i32.add (local.get $sum)
                            (i32.load8_u (i32.add (i32.const 100) (local.get $i)))))
                    (local.set $i (i32.add (local.get $i) (i32.const 1)))
                    (br $lr)))
            (local.get $sum))
    )
"#;

struct OpsInstance {
    store: Store<()>,
    instance: wasmtime::Instance,
}

fn make_ops(an_enabled: bool) -> wasmtime::Result<OpsInstance> {
    make_ops_with(an_enabled, None)
}

fn make_ops_with(an_enabled: bool, an_constant: Option<u64>) -> wasmtime::Result<OpsInstance> {
    let mut config = Config::new();
    config.an_encoding(an_enabled);
    if let Some(a) = an_constant {
        config.an_constant(a);
    }
    let engine = Engine::new(&config)?;
    let module = Module::new(&engine, OPS_WAT)?;
    let mut store = Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
    Ok(OpsInstance { store, instance })
}

fn call2(ops: &mut OpsInstance, name: &str, a: i32, b: i32) -> wasmtime::Result<i32> {
    let f = ops
        .instance
        .get_typed_func::<(i32, i32), i32>(&mut ops.store, name)?;
    f.call(&mut ops.store, (a, b))
}

fn call1(ops: &mut OpsInstance, name: &str, a: i32) -> wasmtime::Result<i32> {
    let f = ops
        .instance
        .get_typed_func::<i32, i32>(&mut ops.store, name)?;
    f.call(&mut ops.store, a)
}

fn ops_assertions(o: &mut OpsInstance) -> wasmtime::Result<()> {

    // S2 — i32.add / i32.sub
    assert_eq!(call2(o, "add", 7, 5)?, 12, "add small");
    assert_eq!(call2(o, "add", 1_000_000, 2_000_000)?, 3_000_000, "add big");
    assert_eq!(call2(o, "sub", 10, 3)?, 7, "sub positive");
    assert_eq!(call2(o, "sub", 3, 10)?, -7, "sub negative result");
    assert_eq!(call2(o, "sub", 100, 200)?, -100, "sub negative result big");

    // S3 — i32.mul (also covers decode-compute-encode path)
    assert_eq!(call2(o, "mul", 7, 6)?, 42, "mul small");
    assert_eq!(call2(o, "mul", 0, 123)?, 0, "mul zero");
    assert_eq!(call2(o, "mul", -3, 4)?, -12, "mul negative");

    // S4 / S5 — i32.div_u / i32.rem_u
    assert_eq!(call2(o, "divu", 20, 3)?, 6, "divu");
    assert_eq!(call2(o, "divu", 100, 7)?, 14, "divu");
    assert_eq!(call2(o, "remu", 20, 3)?, 2, "remu");
    assert_eq!(call2(o, "remu", 100, 7)?, 2, "remu");

    // S1 — i32.const (mixed with add)
    assert_eq!(call1(o, "addconst", 50)?, 150, "i32.const + add");
    assert_eq!(call1(o, "addconst", 0)?, 100, "i32.const only");

    // S6 — comparisons + eqz
    assert_eq!(call2(o, "lt_u", 3, 5)?, 1, "lt_u true");
    assert_eq!(call2(o, "lt_u", 5, 3)?, 0, "lt_u false");
    assert_eq!(call2(o, "ge_u", 5, 5)?, 1, "ge_u eq");
    assert_eq!(call2(o, "ge_u", 2, 5)?, 0, "ge_u false");
    assert_eq!(call2(o, "gt_u", 5, 3)?, 1, "gt_u true");
    assert_eq!(call2(o, "eq", 7, 7)?, 1, "eq true");
    assert_eq!(call2(o, "eq", 7, 8)?, 0, "eq false");
    assert_eq!(call2(o, "ne", 7, 8)?, 1, "ne true");
    assert_eq!(call1(o, "eqz", 0)?, 1, "eqz true");
    assert_eq!(call1(o, "eqz", 7)?, 0, "eqz false");

    // br_if + if/else exercise (encoded condition fed into branch)
    assert_eq!(call2(o, "max_u", 3, 7)?, 7, "max_u");
    assert_eq!(call2(o, "max_u", 9, 4)?, 9, "max_u");

    // loop with encoded counter + accumulator + const
    assert_eq!(call1(o, "loop_count", 10)?, 45, "loop 0..10");
    assert_eq!(call1(o, "loop_count", 100)?, 4950, "loop 0..100");

    // div_u in a loop (digit count)
    assert_eq!(call1(o, "digits", 0)?, 0, "digits 0");
    assert_eq!(call1(o, "digits", 5)?, 1, "digits 5");
    assert_eq!(call1(o, "digits", 99)?, 2, "digits 99");
    assert_eq!(call1(o, "digits", 12345)?, 5, "digits 12345");

    // S7 / S8 — load/store
    assert_eq!(call2(o, "store_load_i32", 64, 12345)?, 12345, "i32 store/load");
    assert_eq!(call2(o, "store_load_i32", 256, -42)?, -42, "i32 store/load neg");
    assert_eq!(call2(o, "store_load_byte", 64, 200)?, 200, "byte store/load");
    assert_eq!(call1(o, "sum_bytes", 10)?, 45, "sum 0..10 via memory");
    assert_eq!(call1(o, "sum_bytes", 20)?, 190, "sum 0..20 via memory");

    Ok(())
}

fn ops_check(an_enabled: bool) -> wasmtime::Result<()> {
    let mut o = make_ops(an_enabled)?;
    ops_assertions(&mut o)
}

#[test]
fn ops_without_an() -> wasmtime::Result<()> {
    ops_check(false)
}

#[test]
fn ops_with_an() -> wasmtime::Result<()> {
    ops_check(true)
}

// Ops still produce identical results across several non-default values of
// the AN constant `A`. Picks: 1 (degenerate identity encoding), 7 (small
// odd), 1009 (small prime), 16777213 (largest 24-bit prime).
#[test]
fn ops_with_an_custom_constants() -> wasmtime::Result<()> {
    for &a in &[1u64, 7, 1009, 16_777_213] {
        let mut o = make_ops_with(true, Some(a))?;
        ops_assertions(&mut o).map_err(|e| {
            wasmtime::Error::msg(format!("ops_assertions failed with A={a}: {e}"))
        })?;
    }
    Ok(())
}
