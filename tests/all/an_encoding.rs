use wasmtime::{Config, Engine, Linker, Module, Store};

const MUL_WAT: &str = include_str!("../../an_encoding/mul.wat");

fn make_config(an_enabled: bool) -> Config {
    let mut config = Config::new();
    config.an_encoding(an_enabled);
    config
}

fn run_mul(an_enabled: bool, a: i32, b: i32) -> wasmtime::Result<i32> {
    let engine = Engine::new(&make_config(an_enabled))?;
    let module = Module::new(&engine, MUL_WAT)?;
    let mut store = Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
    let mul = instance.get_typed_func::<(i32, i32), i32>(&mut store, "mul")?;
    mul.call(&mut store, (a, b))
}

fn check(an_enabled: bool) -> wasmtime::Result<()> {
    assert_eq!(run_mul(an_enabled, 7, 6)?, 42);
    assert_eq!(run_mul(an_enabled, 0, 123)?, 0);
    assert_eq!(run_mul(an_enabled, -3, 4)?, -12);
    assert_eq!(run_mul(an_enabled, 1 << 29, 3)?, 1_610_612_736);
    assert_eq!(run_mul(an_enabled, i32::MAX, 4)?, -4);
    assert_eq!(run_mul(an_enabled, 1 << 29, -3)?, -1_610_612_736);
    assert_eq!(run_mul(an_enabled, i32::MAX, -4)?, 4);
    assert_eq!(run_mul(an_enabled, i32::MAX, i32::MAX)?, 1);
    Ok(())
}

#[test]
fn mul_without_an() -> wasmtime::Result<()> {
    check(false)
}

#[test]
fn mul_with_an() -> wasmtime::Result<()> {
    check(true)
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
// itself lives in `an_encoding/ops.wat` so it can be inspected independently
// of the test harness.
const OPS_WAT: &str = include_str!("../../an_encoding/ops.wat");

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

    // signed comparisons — cover both halves of the i32 sign domain plus
    // the boundary values that exercise the encoded bias-remap.
    let signed_pairs: &[(i32, i32)] = &[
        (3, 5),
        (5, 3),
        (5, 5),
        (-1, 0),
        (0, -1),
        (-1, -1),
        (-5, -3),
        (-3, -5),
        (i32::MIN, 0),
        (0, i32::MIN),
        (i32::MIN, i32::MAX),
        (i32::MAX, i32::MIN),
        (i32::MIN, i32::MIN),
        (i32::MAX, i32::MAX),
        (i32::MAX, -1),
        (-1, i32::MAX),
        (i32::MIN, i32::MIN + 1),
        (i32::MAX - 1, i32::MAX),
    ];
    for &(a, b) in signed_pairs {
        let lt = (a < b) as i32;
        let le = (a <= b) as i32;
        let gt = (a > b) as i32;
        let ge = (a >= b) as i32;
        assert_eq!(call2(o, "lt_s", a, b)?, lt, "lt_s({a},{b})");
        assert_eq!(call2(o, "le_s", a, b)?, le, "le_s({a},{b})");
        assert_eq!(call2(o, "gt_s", a, b)?, gt, "gt_s({a},{b})");
        assert_eq!(call2(o, "ge_s", a, b)?, ge, "ge_s({a},{b})");
    }

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
    assert_eq!(
        call2(o, "store_load_i32", 64, 12345)?,
        12345,
        "i32 store/load"
    );
    assert_eq!(
        call2(o, "store_load_i32", 256, -42)?,
        -42,
        "i32 store/load neg"
    );
    assert_eq!(
        call2(o, "store_load_byte", 64, 200)?,
        200,
        "byte store/load"
    );
    assert_eq!(call1(o, "sum_bytes", 10)?, 45, "sum 0..10 via memory");
    assert_eq!(call1(o, "sum_bytes", 20)?, 190, "sum 0..20 via memory");

    // bitwise logical (LUT-based under AN). Cover zero-arg, identity-arg,
    // small values, full-byte boundaries, all-ones and chunk-crossing
    // patterns so each of the four 8-bit chunks is exercised.
    let bw_pairs: &[(i32, i32)] = &[
        (0, 0),
        (0, 0x12345678u32 as i32),
        (0x12345678u32 as i32, 0),
        (0x12345678u32 as i32, 0x0FF00FF0u32 as i32),
        (0xFFFFFFFFu32 as i32, 0xAAAAAAAAu32 as i32),
        (0xDEADBEEFu32 as i32, 0xCAFEBABEu32 as i32),
        (0x0000FFFFu32 as i32, 0xFFFF0000u32 as i32),
        (0x00FF00FFu32 as i32, 0xFF00FF00u32 as i32),
        (-1, 0x5A5A5A5Au32 as i32),
        (i32::MIN, i32::MAX),
        (0x80000000u32 as i32, 0x7FFFFFFFu32 as i32),
    ];
    for &(a, b) in bw_pairs {
        let expect_and = (a as u32 & b as u32) as i32;
        let expect_or = (a as u32 | b as u32) as i32;
        let expect_xor = (a as u32 ^ b as u32) as i32;
        assert_eq!(
            call2(o, "and", a, b)?,
            expect_and,
            "and({a:#010x},{b:#010x})"
        );
        assert_eq!(call2(o, "or", a, b)?, expect_or, "or({a:#010x},{b:#010x})");
        assert_eq!(
            call2(o, "xor", a, b)?,
            expect_xor,
            "xor({a:#010x},{b:#010x})"
        );
    }

    // unary not via xor -1
    for &v in &[0i32, 1, -1, 0x12345678u32 as i32, i32::MIN, i32::MAX] {
        assert_eq!(call1(o, "not", v)?, !v, "not({v:#010x})");
    }

    // Combined mask/merge — ensures chained bitwise + const + mask behaves.
    let a = 0xAABBCCDDu32 as i32;
    let b = 0x11223344u32 as i32;
    let merged =
        ((a as u32 & 0x00ffff00u32) | (b as u32 & 0xff0000ffu32)) as i32;
    assert_eq!(call2(o, "mask_merge", a, b)?, merged, "mask_merge");

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
        ops_assertions(&mut o)
            .map_err(|e| wasmtime::Error::msg(format!("ops_assertions failed with A={a}: {e}")))?;
    }
    Ok(())
}
