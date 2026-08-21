use wasmtime::{Config, Engine, Linker, Module, Store};

const MUL_WAT: &str = include_str!("../../an_encoding/mul.wat");

fn make_config(an_enabled: bool) -> Config {
    let mut config = Config::new();
    config.an_encoding(an_enabled);
    config
}

#[test]
fn runtime_integer_load_tracker_classifies_dynamic_accesses() -> wasmtime::Result<()> {
    let engine = Engine::new(&make_config(true))?;
    let module = Module::new(
        &engine,
        r#"
            (module
              (memory 1)
              (func (export "run") (param $n i32)
                (block $done
                  (loop $again
                    local.get $n
                    i32.eqz
                    br_if $done

                    i32.const 0
                    i32.load
                    drop

                    i32.const 1
                    i32.load
                    drop

                    i32.const 2
                    i32.load8_u
                    drop

                    i32.const 3
                    i32.load16_u
                    drop

                    i32.const 4
                    i64.load
                    drop

                    i32.const 5
                    i64.load
                    drop

                    i32.const 6
                    i64.load8_u
                    drop

                    i32.const 7
                    i64.load16_u
                    drop

                    i32.const 8
                    i64.load32_u
                    drop

                    i32.const 9
                    i64.load32_u
                    drop

                    local.get $n
                    i32.const 1
                    i32.sub
                    local.tee $n
                    br_if $again))))
        "#,
    )?;
    let mut store = Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
    let run = instance.get_typed_func::<i32, ()>(&mut store, "run")?;

    run.call(&mut store, 10_000)?;

    let expected = if wasmtime_environ::AN_INTEGER_LOAD_TRACKING_ENABLED {
        [10_000; 10]
    } else {
        [0; 10]
    };
    assert_eq!(store.an_integer_load_stats_for_test(), expected);
    Ok(())
}

// AN config pinned to an explicit constant when `a` is `Some`, else the default.
fn an_cfg(an_enabled: bool, a: Option<u64>) -> Config {
    let mut config = make_config(an_enabled);
    if let Some(a) = a {
        config.an_constant(a);
    }
    config
}

// Non-default A values swept by the i64 `*_various_an_constants` tests: 1
// (degenerate identity encoding), 7 (small odd), 1009 (small prime), 2^24−1
// (largest legal A under the u32-LUT bound).
const I64_AN_CONSTANTS: [u64; 4] = [1, 7, 1009, 16_777_215];

// Assert a typed `Result<i64>` trapped with exactly `expected`.
fn assert_trap_i64(res: wasmtime::Result<i64>, expected: wasmtime::Trap, label: &str) {
    let err = res.expect_err(&format!("{label}: expected trap, got Ok"));
    let trap = err
        .downcast_ref::<wasmtime::Trap>()
        .unwrap_or_else(|| panic!("{label}: not a Trap: {err:?}"));
    assert_eq!(*trap, expected, "{label}: trap code mismatch");
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

// Regression test: `br_table` (the wasm switch / jump table) must decode its
// controlling index under AN-encoding. Before the fix the encoded value (`A*v`)
// was handed straight to `br_table`, so every non-zero index landed out of
// range and fell through to the default target. A Rust `match` over several
// integer values lowers to `br_table`, so this silently broke ordinary
// programs (wrong arm taken, no trap).
const BR_TABLE_WAT: &str = r#"
(module
  (func (export "sw") (param i32) (result i32)
    (block $d (block $c2 (block $c1 (block $c0
      (br_table $c0 $c1 $c2 $d (local.get 0)))
      (return (i32.const 100)))
      (return (i32.const 101)))
      (return (i32.const 102)))
    (i32.const 999)))
"#;

fn run_br_table(an_enabled: bool, sel: i32) -> wasmtime::Result<i32> {
    let engine = Engine::new(&make_config(an_enabled))?;
    let module = Module::new(&engine, BR_TABLE_WAT)?;
    let mut store = Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
    let sw = instance.get_typed_func::<i32, i32>(&mut store, "sw")?;
    sw.call(&mut store, sel)
}

fn br_table_check(an_enabled: bool) -> wasmtime::Result<()> {
    // Non-zero selectors only: at selector 0 a missing decode is invisible
    // (`A*0 == 0` still selects arm 0). 1 and 2 land on distinct arms, proving
    // the selector was decoded; 3 and 7 fall through to the default.
    assert_eq!(run_br_table(an_enabled, 1)?, 101);
    assert_eq!(run_br_table(an_enabled, 2)?, 102);
    assert_eq!(run_br_table(an_enabled, 3)?, 999); // out of range -> default
    assert_eq!(run_br_table(an_enabled, 7)?, 999);
    Ok(())
}

#[test]
fn br_table_without_an() -> wasmtime::Result<()> {
    br_table_check(false)
}

#[test]
fn br_table_with_an() -> wasmtime::Result<()> {
    br_table_check(true)
}

// i64 encoding: `i64.const`, `i64.add`, `i32.wrap_i64`. Each function keeps i64
// internal and returns i32 (via wrap), so it exercises the encoded-i64 pipeline
// (chokepoint widen to I128 + I128 arithmetic). `hi32`/`wrap64` prove the high 32 bits survive
// `i64.add` (an i32-only path would lose them) and that the sum canonicalizes
// at 2^64.
const I64_ADDWRAP_WAT: &str = r#"
(module
  (func (export "lo") (result i32)
    (i32.wrap_i64 (i64.add (i64.const 5) (i64.const 7))))
  (func (export "carry") (result i32)
    (i32.wrap_i64 (i64.add (i64.const 0x1_0000_0001) (i64.const 0xFFFF_FFFF))))
  (func (export "hi32") (result i32)
    (i32.wrap_i64 (i64.add (i64.const 0x1_0000_0000) (i64.const 1))))
  (func (export "wrap64") (result i32)
    (i32.wrap_i64 (i64.add (i64.const 0xFFFF_FFFF_FFFF_FFFF) (i64.const 2)))))
"#;

fn run_i64_addwrap(an_enabled: bool, a: Option<u64>, func: &str) -> wasmtime::Result<i32> {
    let engine = Engine::new(&an_cfg(an_enabled, a))?;
    let module = Module::new(&engine, I64_ADDWRAP_WAT)?;
    let mut store = Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
    let f = instance.get_typed_func::<(), i32>(&mut store, func)?;
    f.call(&mut store, ())
}

fn i64_addwrap_check(an_enabled: bool, a: Option<u64>) -> wasmtime::Result<()> {
    assert_eq!(run_i64_addwrap(an_enabled, a, "lo")?, 12);
    assert_eq!(run_i64_addwrap(an_enabled, a, "carry")?, 0);
    assert_eq!(run_i64_addwrap(an_enabled, a, "hi32")?, 1);
    assert_eq!(run_i64_addwrap(an_enabled, a, "wrap64")?, 1);
    Ok(())
}

#[test]
fn i64_addwrap_without_an() -> wasmtime::Result<()> {
    i64_addwrap_check(false, None)
}

#[test]
fn i64_addwrap_with_an() -> wasmtime::Result<()> {
    i64_addwrap_check(true, None)
}

// i64 params + result through the wasm/host trampolines: encode raw i64 args to
// I128 on entry, decode the I128 result back to raw i64 on exit. Also exercises
// I128 locals (`local.get`) and the `A*2^64` wraparound canonicalization.
const I64_ADD_WAT: &str = r#"
(module
  (func (export "add64") (param i64 i64) (result i64)
    (i64.add (local.get 0) (local.get 1))))
"#;

fn run_add64(an_enabled: bool, an: Option<u64>, a: i64, b: i64) -> wasmtime::Result<i64> {
    let engine = Engine::new(&an_cfg(an_enabled, an))?;
    let module = Module::new(&engine, I64_ADD_WAT)?;
    let mut store = Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
    let f = instance.get_typed_func::<(i64, i64), i64>(&mut store, "add64")?;
    f.call(&mut store, (a, b))
}

fn add64_check(an_enabled: bool, an: Option<u64>) -> wasmtime::Result<()> {
    assert_eq!(run_add64(an_enabled, an, 5, 7)?, 12);
    assert_eq!(run_add64(an_enabled, an, -1, 1)?, 0);
    assert_eq!(run_add64(an_enabled, an, i64::MIN, i64::MIN)?, 0); // -2^64 ≡ 0
    assert_eq!(run_add64(an_enabled, an, i64::MAX, 1)?, i64::MIN);
    assert_eq!(
        run_add64(an_enabled, an, 1234567890123, 9876543210)?,
        1244444433333
    );
    Ok(())
}

#[test]
fn add64_without_an() -> wasmtime::Result<()> {
    add64_check(false, None)
}

#[test]
fn add64_with_an() -> wasmtime::Result<()> {
    add64_check(true, None)
}

// i64 stays-encoded ops: sub (canonicalize mod A*2^64), signed/unsigned
// compares (the A*2^63 bias remap), eqz, and a narrow sign-extend. Compares
// return wasm i32; sub/ext8 return i64 (through the trampolines).
const I64_OPS_WAT: &str = r#"
(module
  (func (export "sub64") (param i64 i64) (result i64) (i64.sub (local.get 0) (local.get 1)))
  (func (export "lt_s") (param i64 i64) (result i32) (i64.lt_s (local.get 0) (local.get 1)))
  (func (export "lt_u") (param i64 i64) (result i32) (i64.lt_u (local.get 0) (local.get 1)))
  (func (export "le_s") (param i64 i64) (result i32) (i64.le_s (local.get 0) (local.get 1)))
  (func (export "le_u") (param i64 i64) (result i32) (i64.le_u (local.get 0) (local.get 1)))
  (func (export "gt_s") (param i64 i64) (result i32) (i64.gt_s (local.get 0) (local.get 1)))
  (func (export "gt_u") (param i64 i64) (result i32) (i64.gt_u (local.get 0) (local.get 1)))
  (func (export "ge_s") (param i64 i64) (result i32) (i64.ge_s (local.get 0) (local.get 1)))
  (func (export "ge_u") (param i64 i64) (result i32) (i64.ge_u (local.get 0) (local.get 1)))
  (func (export "eq") (param i64 i64) (result i32) (i64.eq (local.get 0) (local.get 1)))
  (func (export "ne") (param i64 i64) (result i32) (i64.ne (local.get 0) (local.get 1)))
  (func (export "eqz") (param i64) (result i32) (i64.eqz (local.get 0)))
  (func (export "ext8") (param i64) (result i64) (i64.extend8_s (local.get 0)))
  (func (export "ext16") (param i64) (result i64) (i64.extend16_s (local.get 0)))
  (func (export "ext32") (param i64) (result i64) (i64.extend32_s (local.get 0))))
"#;

fn i64ops_ii_i64(an: bool, anc: Option<u64>, func: &str, a: i64, b: i64) -> wasmtime::Result<i64> {
    let engine = Engine::new(&an_cfg(an, anc))?;
    let module = Module::new(&engine, I64_OPS_WAT)?;
    let mut store = Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
    let f = instance.get_typed_func::<(i64, i64), i64>(&mut store, func)?;
    f.call(&mut store, (a, b))
}

fn i64ops_ii_i32(an: bool, anc: Option<u64>, func: &str, a: i64, b: i64) -> wasmtime::Result<i32> {
    let engine = Engine::new(&an_cfg(an, anc))?;
    let module = Module::new(&engine, I64_OPS_WAT)?;
    let mut store = Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
    let f = instance.get_typed_func::<(i64, i64), i32>(&mut store, func)?;
    f.call(&mut store, (a, b))
}

fn i64ops_i_i32(an: bool, anc: Option<u64>, func: &str, a: i64) -> wasmtime::Result<i32> {
    let engine = Engine::new(&an_cfg(an, anc))?;
    let module = Module::new(&engine, I64_OPS_WAT)?;
    let mut store = Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
    let f = instance.get_typed_func::<i64, i32>(&mut store, func)?;
    f.call(&mut store, a)
}

fn i64ops_i_i64(an: bool, anc: Option<u64>, func: &str, a: i64) -> wasmtime::Result<i64> {
    let engine = Engine::new(&an_cfg(an, anc))?;
    let module = Module::new(&engine, I64_OPS_WAT)?;
    let mut store = Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
    let f = instance.get_typed_func::<i64, i64>(&mut store, func)?;
    f.call(&mut store, a)
}

fn i64ops_check(an: bool, anc: Option<u64>) -> wasmtime::Result<()> {
    assert_eq!(i64ops_ii_i64(an, anc, "sub64", 7, 5)?, 2);
    assert_eq!(i64ops_ii_i64(an, anc, "sub64", 5, 7)?, -2);
    assert_eq!(i64ops_ii_i64(an, anc, "sub64", i64::MIN, 1)?, i64::MAX); // wrap

    // Full signed/unsigned compare matrix vs a Rust oracle, hitting the
    // `A*2^63` (signed-bias) and `A*2^64` (band) remap boundaries via
    // MIN/MAX/-1 operands. Signed and unsigned must disagree on the high half.
    let pairs: &[(i64, i64)] = &[
        (0, 0),
        (5, 5),
        (-1, 0),
        (0, -1),
        (-1, -1),
        (1, -1),
        (-1, 1),
        (i64::MIN, i64::MAX),
        (i64::MAX, i64::MIN),
        (i64::MIN, i64::MIN),
        (i64::MAX, i64::MAX),
        (i64::MIN, -1),
        (i64::MAX, 1),
        (-100, 7),
        (100, -7),
        (42, 43),
    ];
    for &(x, y) in pairs {
        let (xu, yu) = (x as u64, y as u64);
        let b = |c: bool| c as i32;
        assert_eq!(
            i64ops_ii_i32(an, anc, "lt_s", x, y)?,
            b(x < y),
            "lt_s({x},{y})"
        );
        assert_eq!(
            i64ops_ii_i32(an, anc, "lt_u", x, y)?,
            b(xu < yu),
            "lt_u({x},{y})"
        );
        assert_eq!(
            i64ops_ii_i32(an, anc, "le_s", x, y)?,
            b(x <= y),
            "le_s({x},{y})"
        );
        assert_eq!(
            i64ops_ii_i32(an, anc, "le_u", x, y)?,
            b(xu <= yu),
            "le_u({x},{y})"
        );
        assert_eq!(
            i64ops_ii_i32(an, anc, "gt_s", x, y)?,
            b(x > y),
            "gt_s({x},{y})"
        );
        assert_eq!(
            i64ops_ii_i32(an, anc, "gt_u", x, y)?,
            b(xu > yu),
            "gt_u({x},{y})"
        );
        assert_eq!(
            i64ops_ii_i32(an, anc, "ge_s", x, y)?,
            b(x >= y),
            "ge_s({x},{y})"
        );
        assert_eq!(
            i64ops_ii_i32(an, anc, "ge_u", x, y)?,
            b(xu >= yu),
            "ge_u({x},{y})"
        );
        assert_eq!(
            i64ops_ii_i32(an, anc, "eq", x, y)?,
            b(x == y),
            "eq({x},{y})"
        );
        assert_eq!(
            i64ops_ii_i32(an, anc, "ne", x, y)?,
            b(x != y),
            "ne({x},{y})"
        );
    }

    assert_eq!(i64ops_i_i32(an, anc, "eqz", 0)?, 1);
    assert_eq!(i64ops_i_i32(an, anc, "eqz", 5)?, 0);

    // sign-extend from the byte / halfword / word boundaries; upper bits beyond
    // the extended width are discarded before sign-extension.
    assert_eq!(i64ops_i_i64(an, anc, "ext8", 0xFF)?, -1);
    assert_eq!(i64ops_i_i64(an, anc, "ext8", 0x7F)?, 127);
    assert_eq!(i64ops_i_i64(an, anc, "ext8", 0x100)?, 0);
    assert_eq!(i64ops_i_i64(an, anc, "ext16", 0xFFFF)?, -1);
    assert_eq!(i64ops_i_i64(an, anc, "ext16", 0x8000)?, -32768);
    assert_eq!(i64ops_i_i64(an, anc, "ext16", 0x7FFF)?, 32767);
    assert_eq!(i64ops_i_i64(an, anc, "ext32", 0xFFFF_FFFF)?, -1);
    assert_eq!(i64ops_i_i64(an, anc, "ext32", 0x8000_0000)?, -2147483648);
    assert_eq!(i64ops_i_i64(an, anc, "ext32", 0x7FFF_FFFF)?, 2147483647);
    assert_eq!(
        i64ops_i_i64(an, anc, "ext32", 0xFFFF_FFFF_0000_0001u64 as i64)?,
        1
    );
    Ok(())
}

#[test]
fn i64ops_without_an() -> wasmtime::Result<()> {
    i64ops_check(false, None)
}

#[test]
fn i64ops_with_an() -> wasmtime::Result<()> {
    i64ops_check(true, None)
}

// i64 div/rem via the general i128÷i128 (`emit_udivrem_i128`): signed/unsigned,
// negatives, and the wasm trap cases (/0, INT_MIN/-1).
const I64_DIVREM_WAT: &str = r#"
(module
  (func (export "divu") (param i64 i64) (result i64) (i64.div_u (local.get 0) (local.get 1)))
  (func (export "divs") (param i64 i64) (result i64) (i64.div_s (local.get 0) (local.get 1)))
  (func (export "remu") (param i64 i64) (result i64) (i64.rem_u (local.get 0) (local.get 1)))
  (func (export "rems") (param i64 i64) (result i64) (i64.rem_s (local.get 0) (local.get 1))))
"#;

fn run_divrem(an: bool, anc: Option<u64>, func: &str, a: i64, b: i64) -> wasmtime::Result<i64> {
    let engine = Engine::new(&an_cfg(an, anc))?;
    let module = Module::new(&engine, I64_DIVREM_WAT)?;
    let mut store = Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
    let f = instance.get_typed_func::<(i64, i64), i64>(&mut store, func)?;
    f.call(&mut store, (a, b))
}

fn divrem_check(an: bool, anc: Option<u64>) -> wasmtime::Result<()> {
    // Value matrix vs a Rust oracle: all sign combinations, MIN/MAX, and the
    // -1 dividend (max unsigned). Excludes the trapping pairs (`/0`,
    // `INT_MIN/-1` for div_s), which are asserted separately by trap code.
    let pairs: &[(i64, i64)] = &[
        (7, 2),
        (-7, 2),
        (7, -2),
        (-7, -2),
        (0, 5),
        (0, -5),
        (i64::MIN, 2),
        (i64::MIN, 3),
        (i64::MAX, 1),
        (i64::MAX, -1),
        (-1, 1),
        (1, -1),
        (-1, -1),
        (-1, i64::MIN),
        (1, i64::MIN),
        (i64::MIN, i64::MIN),
        (i64::MIN, i64::MAX),
        (i64::MAX, i64::MAX),
        (100, 7),
        (-100, 7),
        (100, -7),
        (-100, -7),
        (-1, 10), // (2^64-1) / 10 unsigned
    ];
    for &(x, y) in pairs {
        let (xu, yu) = (x as u64, y as u64);
        assert_eq!(
            run_divrem(an, anc, "divu", x, y)?,
            (xu / yu) as i64,
            "divu({x},{y})"
        );
        assert_eq!(
            run_divrem(an, anc, "remu", x, y)?,
            (xu % yu) as i64,
            "remu({x},{y})"
        );
        assert_eq!(
            run_divrem(an, anc, "divs", x, y)?,
            x.wrapping_div(y),
            "divs({x},{y})"
        );
        assert_eq!(
            run_divrem(an, anc, "rems", x, y)?,
            x.wrapping_rem(y),
            "rems({x},{y})"
        );
    }
    // INT_MIN % -1 == 0 (no trap; the abs trick yields urem(A*2^63, A) == 0).
    assert_eq!(run_divrem(an, anc, "rems", i64::MIN, -1)?, 0);

    // Division by zero traps `IntegerDivisionByZero` for all four ops, any
    // dividend — assert the exact trap code, not just error-vs-ok.
    for &lhs in &[0i64, 1, -1, 42, i64::MIN, i64::MAX] {
        for func in ["divu", "divs", "remu", "rems"] {
            assert_trap_i64(
                run_divrem(an, anc, func, lhs, 0),
                wasmtime::Trap::IntegerDivisionByZero,
                &format!("{func}({lhs}/0)"),
            );
        }
    }
    // INT_MIN / -1 overflows `IntegerOverflow` (div_s only; rem_s above is 0).
    assert_trap_i64(
        run_divrem(an, anc, "divs", i64::MIN, -1),
        wasmtime::Trap::IntegerOverflow,
        "divs INT_MIN/-1",
    );
    Ok(())
}

#[test]
fn divrem_without_an() -> wasmtime::Result<()> {
    divrem_check(false, None)
}

#[test]
fn divrem_with_an() -> wasmtime::Result<()> {
    divrem_check(true, None)
}

// i64 bitwise (8-chunk LUT, I128 accumulator) and bit-counts (decode/native/
// re-encode). Bitwise values span all eight bytes to exercise every chunk.
const I64_BIT_WAT: &str = r#"
(module
  (func (export "and") (param i64 i64) (result i64) (i64.and (local.get 0) (local.get 1)))
  (func (export "or") (param i64 i64) (result i64) (i64.or (local.get 0) (local.get 1)))
  (func (export "xor") (param i64 i64) (result i64) (i64.xor (local.get 0) (local.get 1)))
  (func (export "clz") (param i64) (result i64) (i64.clz (local.get 0)))
  (func (export "ctz") (param i64) (result i64) (i64.ctz (local.get 0)))
  (func (export "popcnt") (param i64) (result i64) (i64.popcnt (local.get 0))))
"#;

fn run_bit_ii(an: bool, anc: Option<u64>, func: &str, a: i64, b: i64) -> wasmtime::Result<i64> {
    let engine = Engine::new(&an_cfg(an, anc))?;
    let module = Module::new(&engine, I64_BIT_WAT)?;
    let mut store = Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
    let f = instance.get_typed_func::<(i64, i64), i64>(&mut store, func)?;
    f.call(&mut store, (a, b))
}

fn run_bit_i(an: bool, anc: Option<u64>, func: &str, a: i64) -> wasmtime::Result<i64> {
    let engine = Engine::new(&an_cfg(an, anc))?;
    let module = Module::new(&engine, I64_BIT_WAT)?;
    let mut store = Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
    let f = instance.get_typed_func::<i64, i64>(&mut store, func)?;
    f.call(&mut store, a)
}

fn bitwise_check(an: bool, anc: Option<u64>) -> wasmtime::Result<()> {
    // and/or/xor vs Rust oracle over operand pairs whose bytes differ across
    // every one of the eight 8-bit LUT chunks.
    let pairs: &[(i64, i64)] = &[
        (
            0x0123_4567_89AB_CDEFu64 as i64,
            0xFEDC_BA98_7654_3210u64 as i64,
        ),
        (
            0x0123_4567_89AB_CDEFu64 as i64,
            0xFFFF_FFFF_0000_0000u64 as i64,
        ),
        (0x0123_4567_89AB_CDEFu64 as i64, -1),
        (-1, 0),
        (0x0123_4567_89AB_CDEFu64 as i64, 0),
        (
            0xFF00_FF00_FF00_FF00u64 as i64,
            0x00FF_00FF_00FF_00FFu64 as i64,
        ),
        (
            0x8040_2010_0804_0201u64 as i64,
            0x0102_0408_1020_4080u64 as i64,
        ),
        (i64::MIN, i64::MAX),
    ];
    for &(x, y) in pairs {
        assert_eq!(
            run_bit_ii(an, anc, "and", x, y)?,
            x & y,
            "and({x:#x},{y:#x})"
        );
        assert_eq!(run_bit_ii(an, anc, "or", x, y)?, x | y, "or({x:#x},{y:#x})");
        assert_eq!(
            run_bit_ii(an, anc, "xor", x, y)?,
            x ^ y,
            "xor({x:#x},{y:#x})"
        );
    }
    // clz/ctz/popcnt vs oracle across bit patterns incl. chunk-crossing ones.
    let unary: &[i64] = &[
        0,
        1,
        -1,
        i64::MIN,
        i64::MAX,
        0x0123_4567_89AB_CDEFu64 as i64,
        0x0000_FFFF_FFFF_0000u64 as i64,
        0xFFFF_0000_0000_FFFFu64 as i64,
        0x8000_0000_0000_0000u64 as i64,
        0x0000_0000_0000_0100u64 as i64,
    ];
    for &v in unary {
        let u = v as u64;
        assert_eq!(
            run_bit_i(an, anc, "clz", v)?,
            u.leading_zeros() as i64,
            "clz({v:#x})"
        );
        assert_eq!(
            run_bit_i(an, anc, "ctz", v)?,
            u.trailing_zeros() as i64,
            "ctz({v:#x})"
        );
        assert_eq!(
            run_bit_i(an, anc, "popcnt", v)?,
            u.count_ones() as i64,
            "popcnt({v:#x})"
        );
    }
    Ok(())
}

#[test]
fn i64_bitwise_without_an() -> wasmtime::Result<()> {
    bitwise_check(false, None)
}

#[test]
fn i64_bitwise_with_an() -> wasmtime::Result<()> {
    bitwise_check(true, None)
}

// i64 shifts/rotates (stays-encoded; count decoded + masked &63).
const I64_SHIFT_WAT: &str = r#"
(module
  (func (export "shl") (param i64 i64) (result i64) (i64.shl (local.get 0) (local.get 1)))
  (func (export "shru") (param i64 i64) (result i64) (i64.shr_u (local.get 0) (local.get 1)))
  (func (export "shrs") (param i64 i64) (result i64) (i64.shr_s (local.get 0) (local.get 1)))
  (func (export "rotl") (param i64 i64) (result i64) (i64.rotl (local.get 0) (local.get 1)))
  (func (export "rotr") (param i64 i64) (result i64) (i64.rotr (local.get 0) (local.get 1))))
"#;

fn run_shift(an: bool, anc: Option<u64>, func: &str, v: i64, k: i64) -> wasmtime::Result<i64> {
    let engine = Engine::new(&an_cfg(an, anc))?;
    let module = Module::new(&engine, I64_SHIFT_WAT)?;
    let mut store = Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
    let f = instance.get_typed_func::<(i64, i64), i64>(&mut store, func)?;
    f.call(&mut store, (v, k))
}

fn shift_check(an: bool, anc: Option<u64>) -> wasmtime::Result<()> {
    // Value × count matrix vs a Rust oracle for all five ops. Counts include
    // wraparound values (>= 64) to exercise the `&63` masking on every op, not
    // just shl/rotl. Values include negatives (high-bit set) for shr_s/rotr.
    let vals: &[i64] = &[
        0,
        1,
        -1,
        i64::MIN,
        i64::MAX,
        0x0123_4567_89AB_CDEFu64 as i64,
        0xFF,
        8,
    ];
    let counts: &[i64] = &[0, 1, 3, 5, 31, 32, 33, 60, 63, 64, 65, 127, 128];
    for &v in vals {
        let u = v as u64;
        for &k in counts {
            let s = (k & 63) as u32; // wasm masks the shift count to 6 bits
            assert_eq!(
                run_shift(an, anc, "shl", v, k)?,
                (u << s) as i64,
                "shl({v:#x},{k})"
            );
            assert_eq!(
                run_shift(an, anc, "shru", v, k)?,
                (u >> s) as i64,
                "shru({v:#x},{k})"
            );
            assert_eq!(
                run_shift(an, anc, "shrs", v, k)?,
                v >> s,
                "shrs({v:#x},{k})"
            );
            assert_eq!(
                run_shift(an, anc, "rotl", v, k)?,
                u.rotate_left(s) as i64,
                "rotl({v:#x},{k})"
            );
            assert_eq!(
                run_shift(an, anc, "rotr", v, k)?,
                u.rotate_right(s) as i64,
                "rotr({v:#x},{k})"
            );
        }
    }
    Ok(())
}

#[test]
fn i64_shift_without_an() -> wasmtime::Result<()> {
    shift_check(false, None)
}

#[test]
fn i64_shift_with_an() -> wasmtime::Result<()> {
    shift_check(true, None)
}

// i64 multiply: stays-encoded, traps on 128-bit product overflow. Non-overflow
// cases (incl. 2^64 wraparound of the low result) match AN-off; the overflow
// case is the one intentional AN-on/AN-off divergence (AN-on traps).
const I64_MUL_WAT: &str = r#"
(module
  (func (export "mul") (param i64 i64) (result i64) (i64.mul (local.get 0) (local.get 1))))
"#;

fn run_mul64(an: bool, anc: Option<u64>, a: i64, b: i64) -> wasmtime::Result<i64> {
    let engine = Engine::new(&an_cfg(an, anc))?;
    let module = Module::new(&engine, I64_MUL_WAT)?;
    let mut store = Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
    let f = instance.get_typed_func::<(i64, i64), i64>(&mut store, "mul")?;
    f.call(&mut store, (a, b))
}

fn mul64_check(an: bool, anc: Option<u64>) -> wasmtime::Result<()> {
    // Non-overflowing products: the stays-encoded result matches the
    // wasm-wrapped value for every A (`A^2 * n*m` stays within the 128-bit
    // product band). Each pair keeps at least one small-magnitude operand —
    // two near-2^64 unsigned operands (e.g. negative * negative) would push
    // `A^2 * n*m` past 2^128 and legitimately trap (see `mul64_overflow_*`).
    let pairs: &[(i64, i64)] = &[
        (6, 7),
        (0, 123),
        (-3, 4),
        (-7, 3),
        (1234567, 7654321),
        (0x1_0000_0000, 2),
        (0x1_0000_0000, 0x1_0000_0000), // 2^64 wraps to 0
        (0x1_0000_0001, 0x1_0000_0000), // wraps to 2^32
        (i64::MAX, 1),
        (-1, 1),
        (0xDEAD_BEEF, 0x1_2345),
    ];
    for &(x, y) in pairs {
        assert_eq!(
            run_mul64(an, anc, x, y)?,
            x.wrapping_mul(y),
            "mul({x:#x},{y:#x})"
        );
    }
    Ok(())
}

#[test]
fn mul64_without_an() -> wasmtime::Result<()> {
    mul64_check(false, None)
}

#[test]
fn mul64_with_an() -> wasmtime::Result<()> {
    mul64_check(true, None)
}

#[test]
fn mul64_overflow_traps_under_an() -> wasmtime::Result<()> {
    // (1<<62)^2 = 2^124. The stays-encoded product is `A^2 * 2^124`; for any
    // A > 4 that exceeds 2^128 and traps `AnI64WidenOverflow`. AN-off computes
    // the wasm-wrapped result (2^124 mod 2^64 == 0). Swept across the legal A
    // values > 4 (A=1 cannot overflow: `n*m < 2^128` always holds, so it is
    // excluded — see `mul64_no_overflow_with_an_constant_1`).
    let big = 1i64 << 62;
    assert_eq!(run_mul64(false, None, big, big)?, 0);
    for &a in &[7u64, 1009, 65521, 16_777_215] {
        assert_trap_i64(
            run_mul64(true, Some(a), big, big),
            wasmtime::Trap::AnI64WidenOverflow,
            &format!("mul64 overflow A={a}"),
        );
    }
    Ok(())
}

#[test]
fn mul64_no_overflow_with_an_constant_1() -> wasmtime::Result<()> {
    // With A=1 the encoding is the identity, so `A^2 * n*m == n*m < 2^128`
    // never overflows the 128-bit product: even the maximal product must NOT
    // trap and must match the wasm-wrapped result.
    let big = 1i64 << 62;
    assert_eq!(run_mul64(true, Some(1), big, big)?, 0);
    assert_eq!(run_mul64(true, Some(1), -1, -1)?, 1); // (2^64-1)^2 mod 2^64
    Ok(())
}

// i64 linear-memory store/load: a store decodes I128->raw i64 then reuses the
// two-i32-slot shadow decomposition; a load verifies all touched 4-byte slots
// (an unaligned 8-byte span hits three) then re-encodes. Stores and loads are
// separate exports so narrow stores can be exercised independently.
const I64_MEM_WAT: &str = r#"
(module
  (memory (export "m") 1)
  (func (export "store64") (param i32 i64) (i64.store (local.get 0) (local.get 1)))
  (func (export "load64") (param i32) (result i64) (i64.load (local.get 0)))
  (func (export "store32") (param i32 i64) (i64.store32 (local.get 0) (local.get 1)))
  (func (export "load32u") (param i32) (result i64) (i64.load32_u (local.get 0)))
  (func (export "load32s") (param i32) (result i64) (i64.load32_s (local.get 0)))
  (func (export "store16") (param i32 i64) (i64.store16 (local.get 0) (local.get 1)))
  (func (export "load16u") (param i32) (result i64) (i64.load16_u (local.get 0)))
  (func (export "load16s") (param i32) (result i64) (i64.load16_s (local.get 0)))
  (func (export "store8") (param i32 i64) (i64.store8 (local.get 0) (local.get 1)))
  (func (export "load8u") (param i32) (result i64) (i64.load8_u (local.get 0)))
  (func (export "load8s") (param i32) (result i64) (i64.load8_s (local.get 0))))
"#;

// Store `val` via `store_fn` at `addr`, then read it back via `load_fn` on the
// same instance.
fn mem_store_load(
    an: bool,
    anc: Option<u64>,
    store_fn: &str,
    load_fn: &str,
    addr: i32,
    val: i64,
) -> wasmtime::Result<i64> {
    let engine = Engine::new(&an_cfg(an, anc))?;
    let module = Module::new(&engine, I64_MEM_WAT)?;
    let mut store = Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
    let sf = instance.get_typed_func::<(i32, i64), ()>(&mut store, store_fn)?;
    let lf = instance.get_typed_func::<i32, i64>(&mut store, load_fn)?;
    sf.call(&mut store, (addr, val))?;
    lf.call(&mut store, addr)
}

fn mem_check(an: bool, anc: Option<u64>) -> wasmtime::Result<()> {
    let v = 0x0123_4567_89AB_CDEFu64 as i64;
    // Full 8-byte store/load over aligned, unaligned, and cross-slot (the
    // unaligned 8-byte span straddles three shadow slots) offsets, across a
    // value set that exercises high-bit, all-ones, zero, and MIN/MAX.
    for addr in [0i32, 8, 16, 1, 2, 3, 4, 5, 6, 7] {
        for val in [
            v,
            -1,
            0,
            i64::MIN,
            i64::MAX,
            0x00FF_00FF_00FF_00FFu64 as i64,
        ] {
            assert_eq!(
                mem_store_load(an, anc, "store64", "load64", addr, val)?,
                val,
                "store64/load64 addr={addr} val={val:#x}"
            );
        }
    }
    // Narrow stores at aligned + unaligned + cross-slot offsets, read back via
    // the matching unsigned/signed narrow loads. Oracle = the low bits of `v`,
    // sign-extended for the `*_s` loads (`v`'s low byte/half/word all have their
    // top bit set).
    for addr in [0i32, 1, 3, 7, 8, 13] {
        assert_eq!(
            mem_store_load(an, anc, "store32", "load32u", addr, v)?,
            0x89AB_CDEF,
            "store32/load32u addr={addr}"
        );
        assert_eq!(
            mem_store_load(an, anc, "store32", "load32s", addr, v)?,
            0xFFFF_FFFF_89AB_CDEFu64 as i64,
            "store32/load32s addr={addr}"
        );
        assert_eq!(
            mem_store_load(an, anc, "store16", "load16u", addr, v)?,
            0xCDEF,
            "store16/load16u addr={addr}"
        );
        assert_eq!(
            mem_store_load(an, anc, "store16", "load16s", addr, v)?,
            0xFFFF_FFFF_FFFF_CDEFu64 as i64,
            "store16/load16s addr={addr}"
        );
        assert_eq!(
            mem_store_load(an, anc, "store8", "load8u", addr, v)?,
            0xEF,
            "store8/load8u addr={addr}"
        );
        assert_eq!(
            mem_store_load(an, anc, "store8", "load8s", addr, v)?,
            0xFFFF_FFFF_FFFF_FFEFu64 as i64,
            "store8/load8s addr={addr}"
        );
    }
    Ok(())
}

#[test]
fn i64_mem_without_an() -> wasmtime::Result<()> {
    mem_check(false, None)
}

#[test]
fn i64_mem_with_an() -> wasmtime::Result<()> {
    mem_check(true, None)
}

#[test]
fn i64_load_validity_check_traps_on_raw_tamper() -> wasmtime::Result<()> {
    // An i64 load verifies BOTH 4-byte shadow slots of its 8-byte span.
    // Tampering a raw byte in either half (after a consistent store, via the
    // untracked `data_ptr` path) must surface as `AnMemoryMismatch` at the
    // load — the i64 counterpart of `load_validity_check_traps_on_raw_tamper`.
    for tamper_off in [9usize, 13] {
        let engine = Engine::new(&make_config(true))?;
        let module = Module::new(&engine, I64_MEM_WAT)?;
        let mut store = Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
        let mem = instance.get_memory(&mut store, "m").expect("memory export");
        let store64 = instance.get_typed_func::<(i32, i64), ()>(&mut store, "store64")?;
        let load64 = instance.get_typed_func::<i32, i64>(&mut store, "load64")?;
        // Consistent store at offset 8 → shadow slots [8..12) and [12..16) valid.
        store64.call(&mut store, (8, 0x0123_4567_89AB_CDEFu64 as i64))?;
        tamper_raw_byte(&mem, &mut store, tamper_off, |b| b ^ 0x80);
        let res = load64.call(&mut store, 8);
        let err = res.expect_err(&format!("tamper at {tamper_off}: expected trap, got Ok"));
        let trap = err
            .downcast_ref::<wasmtime::Trap>()
            .unwrap_or_else(|| panic!("tamper at {tamper_off}: not a Trap: {err:?}"));
        assert_eq!(
            *trap,
            wasmtime::Trap::AnMemoryMismatch,
            "i64.load second-slot tamper at {tamper_off}"
        );
    }
    Ok(())
}

// i64 globals: mutable (storage widened to I128, get/set pass through encoded)
// and immutable const-folded (must emit the encoded `A*v` immediate).
const I64_GLOBAL_WAT: &str = r#"
(module
  (global $g (mut i64) (i64.const 100))
  (global $c i64 (i64.const -7))
  (func (export "gget") (result i64) (global.get $g))
  (func (export "setget") (param i64) (result i64)
    (global.set $g (local.get 0)) (global.get $g))
  (func (export "ginc") (result i64)
    (global.set $g (i64.add (global.get $g) (i64.const 1))) (global.get $g))
  (func (export "cget") (result i64) (global.get $c)))
"#;

fn i64_global_inst(
    an: bool,
    anc: Option<u64>,
) -> wasmtime::Result<(Store<()>, wasmtime::Instance)> {
    let engine = Engine::new(&an_cfg(an, anc))?;
    let module = Module::new(&engine, I64_GLOBAL_WAT)?;
    let mut store = Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
    Ok((store, instance))
}

fn global_check(an: bool, anc: Option<u64>) -> wasmtime::Result<()> {
    let (mut store, inst) = i64_global_inst(an, anc)?;
    let gget = inst.get_typed_func::<(), i64>(&mut store, "gget")?;
    let setget = inst.get_typed_func::<i64, i64>(&mut store, "setget")?;
    let ginc = inst.get_typed_func::<(), i64>(&mut store, "ginc")?;
    let cget = inst.get_typed_func::<(), i64>(&mut store, "cget")?;
    assert_eq!(gget.call(&mut store, ())?, 100);
    for v in [
        0i64,
        -1,
        i64::MAX,
        i64::MIN,
        0x0123_4567_89AB_CDEFu64 as i64,
    ] {
        assert_eq!(setget.call(&mut store, v)?, v);
    }
    // g is now the last setget value; ginc adds 1.
    let before = gget.call(&mut store, ())?;
    assert_eq!(ginc.call(&mut store, ())?, before.wrapping_add(1));
    assert_eq!(cget.call(&mut store, ())?, -7); // immutable const-folded
    Ok(())
}

#[test]
fn i64_global_without_an() -> wasmtime::Result<()> {
    global_check(false, None)
}

#[test]
fn i64_global_with_an() -> wasmtime::Result<()> {
    global_check(true, None)
}

// Re-run the whole guest-side i64 op/memory/global surface across several
// non-default AN constants. This is the i64 analogue of
// `ops_with_an_custom_constants`: it proves the i64 codegen reads `A` from
// `Tunables` rather than baking the default in — covering, in particular, the
// software I128 long-division helper (div/rem) and the bitwise LUT scaling.
#[test]
fn i64_ops_various_an_constants() -> wasmtime::Result<()> {
    for &a in &I64_AN_CONSTANTS {
        let anc = Some(a);
        let ctx = |e: wasmtime::Error| wasmtime::Error::msg(format!("A={a}: {e}"));
        i64_addwrap_check(true, anc).map_err(ctx)?;
        add64_check(true, anc).map_err(ctx)?;
        i64ops_check(true, anc).map_err(ctx)?;
        divrem_check(true, anc).map_err(ctx)?;
        bitwise_check(true, anc).map_err(ctx)?;
        shift_check(true, anc).map_err(ctx)?;
        mul64_check(true, anc).map_err(ctx)?;
        mem_check(true, anc).map_err(ctx)?;
        global_check(true, anc).map_err(ctx)?;
    }
    Ok(())
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

// `fdstat` contains padding between separately written fields. Those writes
// share a 4-byte AN slot and must be validated/resynced as one dirty union.
#[test]
fn wasi_fdstat_disjoint_same_slot_writes_resync() -> wasmtime::Result<()> {
    let mut config = Config::new();
    config.an_encoding(true);
    let engine = Engine::new(&config)?;
    let module = Module::new(
        &engine,
        r#"
            (module
              (import "wasi_snapshot_preview1" "fd_fdstat_get"
                (func $fd_fdstat_get (param i32 i32) (result i32)))
              (memory (export "memory") 1)
              (func (export "run") (result i32)
                ;; Make every byte differ from fdstat's output so processing
                ;; either field independently would false-report corruption.
                i32.const 64
                i32.const -1
                i32.store
                i32.const 1
                i32.const 64
                call $fd_fdstat_get))
        "#,
    )?;
    let mut linker: Linker<wasmtime_wasi::p1::WasiP1Ctx> = Linker::new(&engine);
    wasmtime_wasi::p1::add_to_linker_sync(&mut linker, |t| t)?;
    let stdout = wasmtime_wasi::p2::pipe::MemoryOutputPipe::new(64);
    let ctx = wasmtime_wasi::WasiCtxBuilder::new()
        .stdout(stdout)
        .build_p1();
    let mut store = Store::new(&engine, ctx);
    let instance = linker.instantiate(&mut store, &module)?;
    let run = instance.get_typed_func::<(), i32>(&mut store, "run")?;
    assert_eq!(run.call(&mut store, ())?, 0);
    Ok(())
}

// WASI roundtrip stress with the mandatory per-load check. WASI `fd_read`
// writes raw input bytes into wasm linear memory via `Memory::data_mut`;
// without the post-host resync libcall the encoded shadow would lag behind
// raw and the next wasm `i32.load8_u` of those bytes (inside `$parse`)
// would trap with `AnMemoryMismatch` under the load-validity check. The
// fact that fib still computes the right output is end-to-end proof that
// the resync hook fires and keeps the shadow in lockstep with raw after
// host writes.
fn run_fib_with_load_check(n: u32) -> wasmtime::Result<String> {
    let mut config = Config::new();
    config.an_encoding(true);
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

#[test]
fn fib_with_an_and_load_validity_check() -> wasmtime::Result<()> {
    // Same answers as `fib_check`, exercising the WASI `fd_read` →
    // post-host resync → wasm load chain with the per-load shadow check on.
    assert_eq!(run_fib_with_load_check(0)?, "0");
    assert_eq!(run_fib_with_load_check(5)?, "5");
    assert_eq!(run_fib_with_load_check(20)?, "6765");
    Ok(())
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

fn call0_r(ops: &mut OpsInstance, name: &str) -> wasmtime::Result<i32> {
    let f = ops
        .instance
        .get_typed_func::<(), i32>(&mut ops.store, name)?;
    f.call(&mut ops.store, ())
}

fn call1_v(ops: &mut OpsInstance, name: &str, a: i32) -> wasmtime::Result<()> {
    let f = ops
        .instance
        .get_typed_func::<i32, ()>(&mut ops.store, name)?;
    f.call(&mut ops.store, a)
}

fn assert_trap(res: wasmtime::Result<i32>, expected: wasmtime::Trap, label: &str) {
    let err = res.expect_err(&format!("{label}: expected trap, got Ok"));
    let trap = err
        .downcast_ref::<wasmtime::Trap>()
        .unwrap_or_else(|| panic!("{label}: not a Trap: {err:?}"));
    assert_eq!(*trap, expected, "{label}: trap code mismatch");
}

fn ops_assertions(o: &mut OpsInstance) -> wasmtime::Result<()> {
    // i32.add / i32.sub
    assert_eq!(call2(o, "add", 7, 5)?, 12, "add small");
    assert_eq!(call2(o, "add", 1_000_000, 2_000_000)?, 3_000_000, "add big");
    assert_eq!(call2(o, "sub", 10, 3)?, 7, "sub positive");
    assert_eq!(call2(o, "sub", 3, 10)?, -7, "sub negative result");
    assert_eq!(call2(o, "sub", 100, 200)?, -100, "sub negative result big");

    // i32.mul (also covers decode-compute-encode path)
    assert_eq!(call2(o, "mul", 7, 6)?, 42, "mul small");
    assert_eq!(call2(o, "mul", 0, 123)?, 0, "mul zero");
    assert_eq!(call2(o, "mul", -3, 4)?, -12, "mul negative");

    // i32.div_u / i32.rem_u
    assert_eq!(call2(o, "divu", 20, 3)?, 6, "divu");
    assert_eq!(call2(o, "divu", 100, 7)?, 14, "divu");
    assert_eq!(call2(o, "remu", 20, 3)?, 2, "remu");
    assert_eq!(call2(o, "remu", 100, 7)?, 2, "remu");

    // signed div / rem — cover all 4 sign combinations, INT_MIN, INT_MAX,
    // /0 trap (both), INT_MIN/-1 trap (div_s only), INT_MIN%-1 == 0 (no trap).
    let signed_div_pairs: &[(i32, i32)] = &[
        (7, 2),
        (-7, 2),
        (7, -2),
        (-7, -2),
        (0, 5),
        (0, -5),
        (i32::MIN, 1),
        (i32::MIN, 2),
        (i32::MIN, 3),
        (i32::MAX, 1),
        (i32::MAX, -1),
        (-1, 1),
        (1, -1),
        (-1, -1),
        (-1, i32::MIN),
        (1, i32::MIN),
        (i32::MIN, i32::MIN),
        (i32::MIN, i32::MAX),
        (i32::MAX, i32::MAX),
    ];
    for &(a, b) in signed_div_pairs {
        assert_eq!(call2(o, "divs", a, b)?, a.wrapping_div(b), "divs({a},{b})");
        assert_eq!(call2(o, "rems", a, b)?, a.wrapping_rem(b), "rems({a},{b})");
    }
    // INT_MIN/-1 traps with INTEGER_OVERFLOW under div_s; rem_s returns 0.
    assert_trap(
        call2(o, "divs", i32::MIN, -1),
        wasmtime::Trap::IntegerOverflow,
        "divs INT_MIN/-1",
    );
    assert_eq!(
        call2(o, "rems", i32::MIN, -1)?,
        0,
        "rems INT_MIN/-1 returns 0"
    );
    // /0 traps with INTEGER_DIVISION_BY_ZERO for both signed and unsigned.
    for &lhs in &[0i32, 1, -1, 42, i32::MIN, i32::MAX] {
        assert_trap(
            call2(o, "divs", lhs, 0),
            wasmtime::Trap::IntegerDivisionByZero,
            &format!("divs {lhs}/0"),
        );
        assert_trap(
            call2(o, "rems", lhs, 0),
            wasmtime::Trap::IntegerDivisionByZero,
            &format!("rems {lhs}%0"),
        );
    }

    // shifts — value-domain × shift-count-domain coverage.
    let shift_vals: &[i32] = &[
        0,
        1,
        -1,
        2,
        42,
        -42,
        0x12345678u32 as i32,
        0xDEADBEEFu32 as i32,
        i32::MIN,
        i32::MAX,
        0x80000001u32 as i32,
        0x7FFFFFFE,
    ];
    // counts span the in-range [0..32) plus wraparound (>= 32) — wasm uses
    // `k mod 32`.
    let shift_counts: &[i32] = &[0, 1, 2, 7, 15, 16, 17, 30, 31, 32, 33, 63, 64, 65];
    for &v in shift_vals {
        for &k in shift_counts {
            let kmod = (k as u32) & 31;
            let expect_shl = ((v as u32).wrapping_shl(kmod)) as i32;
            let expect_shr_u = ((v as u32) >> kmod) as i32;
            let expect_shr_s = v.wrapping_shr(kmod);
            let expect_rotl = (v as u32).rotate_left(kmod) as i32;
            let expect_rotr = (v as u32).rotate_right(kmod) as i32;
            assert_eq!(call2(o, "shl", v, k)?, expect_shl, "shl({v:#010x},{k})");
            assert_eq!(
                call2(o, "shr_u", v, k)?,
                expect_shr_u,
                "shr_u({v:#010x},{k})"
            );
            assert_eq!(
                call2(o, "shr_s", v, k)?,
                expect_shr_s,
                "shr_s({v:#010x},{k})"
            );
            assert_eq!(call2(o, "rotl", v, k)?, expect_rotl, "rotl({v:#010x},{k})");
            assert_eq!(call2(o, "rotr", v, k)?, expect_rotr, "rotr({v:#010x},{k})");
        }
    }

    // clz / ctz / popcnt — boundary plus mixed bit patterns.
    let unary_vals: &[i32] = &[
        0,
        1,
        -1,
        2,
        0x80000000u32 as i32,
        0x7FFFFFFF,
        0x0000FFFFu32 as i32,
        0xFFFF0000u32 as i32,
        0x12345678u32 as i32,
        0xDEADBEEFu32 as i32,
        i32::MIN,
        i32::MAX,
    ];
    for &v in unary_vals {
        let expect_clz = (v as u32).leading_zeros() as i32;
        let expect_ctz = (v as u32).trailing_zeros() as i32;
        let expect_pop = (v as u32).count_ones() as i32;
        assert_eq!(call1(o, "clz", v)?, expect_clz, "clz({v:#010x})");
        assert_eq!(call1(o, "ctz", v)?, expect_ctz, "ctz({v:#010x})");
        assert_eq!(call1(o, "popcnt", v)?, expect_pop, "popcnt({v:#010x})");
    }

    // i32 globals — round-trip including negatives and boundary values.
    assert_eq!(call0_r(o, "g_get")?, 42, "g initial");
    assert_eq!(call0_r(o, "g_neg_get")?, -7, "g_neg initial");
    for &v in &[0i32, 1, -1, 42, -42, i32::MIN, i32::MAX] {
        call1_v(o, "g_set", v)?;
        assert_eq!(call0_r(o, "g_get")?, v, "g round-trip({v})");
    }
    // g_inc combines global.get + add + global.set + global.get in one call.
    call1_v(o, "g_set", 10)?;
    assert_eq!(call1(o, "g_inc", 5)?, 15, "g_inc 10+5");
    assert_eq!(call1(o, "g_inc", -20)?, -5, "g_inc 15-20");
    assert_eq!(call0_r(o, "g_get")?, -5, "g after g_inc");

    // i32.const (mixed with add)
    assert_eq!(call1(o, "addconst", 50)?, 150, "i32.const + add");
    assert_eq!(call1(o, "addconst", 0)?, 100, "i32.const only");

    // comparisons + eqz
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

    // load/store
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
    let merged = ((a as u32 & 0x00ffff00u32) | (b as u32 & 0xff0000ffu32)) as i32;
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
// odd), 1009 (small prime), 16_777_215 (= 2^24 − 1, largest legal A under
// the u32-LUT bound).
#[test]
fn ops_with_an_custom_constants() -> wasmtime::Result<()> {
    for &a in &[1u64, 7, 1009, 16_777_215] {
        let mut o = make_ops_with(true, Some(a))?;
        ops_assertions(&mut o)
            .map_err(|e| wasmtime::Error::msg(format!("ops_assertions failed with A={a}: {e}")))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Host-boundary global encode/decode.
//
// Under AN-encoding an i32 global is stored *encoded* as `A*v`. The guest
// observes the encoded form directly via `global.get`/`global.set`; the host
// is the external boundary, so `Global::get` must decode and `Global::set`
// must encode. These tests cross-check the host view against the guest view to
// prove the two stay in agreement and that the stored form is genuinely
// encoded (not raw).
// ---------------------------------------------------------------------------

// Exports the globals directly (the `ops.wat` module only exports accessor
// functions, so it never exercises the host-side `Global` API).
const GLOBAL_EXPORT_WAT: &str = r#"
(module
  (global $g (export "g") (mut i32) (i32.const 42))
  (global $g_neg (export "g_neg") i32 (i32.const -7))
  (func (export "g_get") (result i32) global.get $g)
  (func (export "g_set") (param i32) local.get 0 global.set $g))
"#;

fn global_boundary_check(an_enabled: bool, an_constant: Option<u64>) -> wasmtime::Result<()> {
    use wasmtime::Val;

    let mut config = Config::new();
    config.an_encoding(an_enabled);
    if let Some(a) = an_constant {
        config.an_constant(a);
    }
    let engine = Engine::new(&config)?;
    let module = Module::new(&engine, GLOBAL_EXPORT_WAT)?;
    let mut store = Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[])?;

    let g = instance.get_global(&mut store, "g").unwrap();
    let g_neg = instance.get_global(&mut store, "g_neg").unwrap();
    let g_get = instance.get_typed_func::<(), i32>(&mut store, "g_get")?;
    let g_set = instance.get_typed_func::<i32, ()>(&mut store, "g_set")?;

    // Host reads decode: the embedder sees the raw initializer, not `A*v`.
    // The guest reads the same value via `global.get`.
    assert_eq!(g.get(&mut store).unwrap_i32(), 42, "host get initial");
    assert_eq!(g_neg.get(&mut store).unwrap_i32(), -7, "host get neg init");
    assert_eq!(g_get.call(&mut store, ())?, 42, "guest get initial");

    let cases = [
        0i32,
        1,
        -1,
        42,
        -42,
        i32::MIN,
        i32::MAX,
        0x7fff_ffff,
        0x8000_0000u32 as i32,
        0x1234_5678,
    ];
    for &v in &cases {
        // Host write encodes; the guest observes the same value.
        g.set(&mut store, Val::I32(v))?;
        assert_eq!(g_get.call(&mut store, ())?, v, "guest sees host-set {v}");
        assert_eq!(g.get(&mut store).unwrap_i32(), v, "host round-trip {v}");

        // Guest write stores encoded; the host decodes the same value back.
        let w = v.wrapping_neg();
        g_set.call(&mut store, w)?;
        assert_eq!(g.get(&mut store).unwrap_i32(), w, "host sees guest-set {w}");
    }
    Ok(())
}

#[test]
fn global_boundary_without_an() -> wasmtime::Result<()> {
    global_boundary_check(false, None)
}

#[test]
fn global_boundary_with_an() -> wasmtime::Result<()> {
    global_boundary_check(true, None)
}

// A host-created (`Global::new`) i32 global imported into an AN module. This
// exercises the `VMGlobalKind::Host` storage path: the host initializer and
// `Global::set` must encode, `Global::get` must decode, and the guest reads
// the encoded form directly.
const GLOBAL_IMPORT_WAT: &str = r#"
(module
  (global $imp (import "env" "g") (mut i32))
  (func (export "get") (result i32) global.get $imp)
  (func (export "inc") (param i32) (result i32)
    global.get $imp local.get 0 i32.add
    global.set $imp
    global.get $imp))
"#;

fn global_import_check(an_enabled: bool, an_constant: Option<u64>) -> wasmtime::Result<()> {
    use wasmtime::{Global, GlobalType, Mutability, Val, ValType};

    let mut config = Config::new();
    config.an_encoding(an_enabled);
    if let Some(a) = an_constant {
        config.an_constant(a);
    }
    let engine = Engine::new(&config)?;
    let module = Module::new(&engine, GLOBAL_IMPORT_WAT)?;
    let mut store = Store::new(&engine, ());

    let g = Global::new(
        &mut store,
        GlobalType::new(ValType::I32, Mutability::Var),
        Val::I32(100),
    )?;
    let instance = wasmtime::Instance::new(&mut store, &module, &[g.into()])?;
    let get = instance.get_typed_func::<(), i32>(&mut store, "get")?;
    let inc = instance.get_typed_func::<i32, i32>(&mut store, "inc")?;

    // Guest reads the host-provided import (host init must have encoded it).
    assert_eq!(
        get.call(&mut store, ())?,
        100,
        "guest reads imported global"
    );
    // Guest mutation flows back to the host decoded.
    assert_eq!(inc.call(&mut store, 23)?, 123, "guest inc");
    assert_eq!(
        g.get(&mut store).unwrap_i32(),
        123,
        "host sees guest mutation"
    );
    // Host write of a negative value is observed by the guest.
    g.set(&mut store, Val::I32(-5))?;
    assert_eq!(get.call(&mut store, ())?, -5, "guest sees host set");
    Ok(())
}

#[test]
fn global_import_without_an() -> wasmtime::Result<()> {
    global_import_check(false, None)
}

#[test]
fn global_import_with_an() -> wasmtime::Result<()> {
    global_import_check(true, None)
}

const GLOBAL_I64_EXPORT_WAT: &str = r#"
(module
  (global $g (export "g") (mut i64) (i64.const 42))
  (global $g_neg (export "g_neg") i64 (i64.const -7))
  (func (export "g_get") (result i64) global.get $g)
  (func (export "g_set") (param i64) local.get 0 global.set $g))
"#;

fn global_i64_boundary_check(an_enabled: bool, an_constant: Option<u64>) -> wasmtime::Result<()> {
    use wasmtime::Val;

    let mut config = Config::new();
    config.an_encoding(an_enabled);
    if let Some(a) = an_constant {
        config.an_constant(a);
    }
    let engine = Engine::new(&config)?;
    let module = Module::new(&engine, GLOBAL_I64_EXPORT_WAT)?;
    let mut store = Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[])?;

    let g = instance.get_global(&mut store, "g").unwrap();
    let g_neg = instance.get_global(&mut store, "g_neg").unwrap();
    let g_get = instance.get_typed_func::<(), i64>(&mut store, "g_get")?;
    let g_set = instance.get_typed_func::<i64, ()>(&mut store, "g_set")?;

    assert_eq!(g.get(&mut store).unwrap_i64(), 42, "host get initial");
    assert_eq!(g_neg.get(&mut store).unwrap_i64(), -7, "host get neg init");
    assert_eq!(g_get.call(&mut store, ())?, 42, "guest get initial");

    let cases = [
        0i64,
        1,
        -1,
        42,
        -42,
        i64::MIN,
        i64::MAX,
        0x0123_4567_89ab_cdef,
        0xfedc_ba98_7654_3210u64 as i64,
    ];
    for &v in &cases {
        g.set(&mut store, Val::I64(v))?;
        assert_eq!(g_get.call(&mut store, ())?, v, "guest sees host-set {v}");
        assert_eq!(g.get(&mut store).unwrap_i64(), v, "host round-trip {v}");

        let w = v.wrapping_neg();
        g_set.call(&mut store, w)?;
        assert_eq!(g.get(&mut store).unwrap_i64(), w, "host sees guest-set {w}");
    }
    Ok(())
}

const GLOBAL_I64_IMPORT_WAT: &str = r#"
(module
  (global $imp (import "env" "g") (mut i64))
  (func (export "get") (result i64) global.get $imp)
  (func (export "inc") (param i64) (result i64)
    global.get $imp local.get 0 i64.add
    global.set $imp
    global.get $imp))
"#;

fn global_i64_import_check(an_enabled: bool, an_constant: Option<u64>) -> wasmtime::Result<()> {
    use wasmtime::{Global, GlobalType, Mutability, Val, ValType};

    let mut config = Config::new();
    config.an_encoding(an_enabled);
    if let Some(a) = an_constant {
        config.an_constant(a);
    }
    let engine = Engine::new(&config)?;
    let module = Module::new(&engine, GLOBAL_I64_IMPORT_WAT)?;
    let mut store = Store::new(&engine, ());

    let g = Global::new(
        &mut store,
        GlobalType::new(ValType::I64, Mutability::Var),
        Val::I64(100),
    )?;
    let instance = wasmtime::Instance::new(&mut store, &module, &[g.into()])?;
    let get = instance.get_typed_func::<(), i64>(&mut store, "get")?;
    let inc = instance.get_typed_func::<i64, i64>(&mut store, "inc")?;

    assert_eq!(
        get.call(&mut store, ())?,
        100,
        "guest reads imported global"
    );
    assert_eq!(inc.call(&mut store, 23)?, 123, "guest inc");
    assert_eq!(
        g.get(&mut store).unwrap_i64(),
        123,
        "host sees guest mutation"
    );
    g.set(&mut store, Val::I64(-5))?;
    assert_eq!(get.call(&mut store, ())?, -5, "guest sees host set");
    Ok(())
}

#[test]
fn global_i64_boundary_without_an() -> wasmtime::Result<()> {
    global_i64_boundary_check(false, None)
}

#[test]
fn global_i64_boundary_with_an() -> wasmtime::Result<()> {
    global_i64_boundary_check(true, None)
}

#[test]
fn global_i64_import_without_an() -> wasmtime::Result<()> {
    global_i64_import_check(false, None)
}

#[test]
fn global_i64_import_with_an() -> wasmtime::Result<()> {
    global_i64_import_check(true, None)
}

// Both boundary paths must hold across several legal values of `A` (the same
// picks as `ops_with_an_custom_constants`), confirming the encode/decode read
// `A` from the tunables rather than baking in the default.
#[test]
fn global_boundary_various_an_constants() -> wasmtime::Result<()> {
    for &a in &[1u64, 7, 1009, 16_777_215] {
        global_boundary_check(true, Some(a))
            .map_err(|e| wasmtime::Error::msg(format!("global_boundary failed with A={a}: {e}")))?;
        global_import_check(true, Some(a))
            .map_err(|e| wasmtime::Error::msg(format!("global_import failed with A={a}: {e}")))?;
        global_i64_boundary_check(true, Some(a)).map_err(|e| {
            wasmtime::Error::msg(format!("global_i64_boundary failed with A={a}: {e}"))
        })?;
        global_i64_import_check(true, Some(a)).map_err(|e| {
            wasmtime::Error::msg(format!("global_i64_import failed with A={a}: {e}"))
        })?;
    }
    Ok(())
}

// Module-level feature refusals: AN-encoding allocates an encoded shadow per
// defined linear memory; shared (atomic) memories are excluded (their shadow
// stores would need atomic-safe paths). Imported non-shared memories are
// supported via the owner-shadow indirection. Each refusal is exercised
// below by compiling a minimal wat module under AN and asserting compile
// fails with a message mentioning AN-encoding.

fn compile_with_config(config: &Config, wat: &str) -> wasmtime::Result<Module> {
    let engine = Engine::new(config)?;
    Module::new(&engine, wat)
}

fn assert_an_refusal(config: &Config, wat: &str, label: &str) {
    let err =
        compile_with_config(config, wat).expect_err(&format!("{label}: expected compile error"));
    let s = format!("{err:#}");
    assert!(
        s.contains("AN-encoding"),
        "{label}: error message did not mention AN-encoding: {s}"
    );
}

fn assert_an_float_refusal(wat: &str, label: &str) {
    let err = compile_with_config(&make_config(true), wat)
        .expect_err(&format!("{label}: expected float refusal under AN"));
    let s = format!("{err:#}");
    assert!(
        s.contains("AN-encoding") && s.contains("floating-point"),
        "{label}: error did not mention float refusal: {s}",
    );
}

#[test]
fn refuse_float_param_under_an() {
    let wat = r#"
        (module
            (func (export "f") (param f32) (result i32) i32.const 0))
    "#;
    assert_an_float_refusal(wat, "f32 param");
}

#[test]
fn refuse_float_result_under_an() {
    let wat = r#"
        (module
            (func (export "f") (result f64) f64.const 0))
    "#;
    assert_an_float_refusal(wat, "f64 result");
}

#[test]
fn refuse_float_local_under_an() {
    let wat = r#"
        (module
            (func (export "f") (result i32)
                (local f32)
                i32.const 0))
    "#;
    assert_an_float_refusal(wat, "f32 local");
}

#[test]
fn refuse_float_global_under_an() {
    let wat = r#"
        (module
            (global $g f64 (f64.const 0))
            (func (export "f") (result i32) i32.const 0))
    "#;
    assert_an_float_refusal(wat, "f64 global");
}

#[test]
fn refuse_float_op_under_an() {
    // No float in sigs/globals/locals — only a transient `f32.const; drop`.
    // Caught by the operator-walk arm.
    let wat = r#"
        (module
            (func (export "f") (result i32)
                f32.const 1.0
                drop
                i32.const 0))
    "#;
    assert_an_float_refusal(wat, "f32.const operator");
}

// Reference types as *values* (externref, funcref-as-value, …) are opaque
// host handles with no integer encoding, so they are refused wherever they
// appear as a value: signatures, globals, locals. `funcref` *tables* are the
// exception (core `call_indirect` dispatch) and stay allowed; any other table
// element type is refused. Each test asserts the error mentions AN-encoding
// and reference types.
fn assert_an_ref_refusal(wat: &str, label: &str) {
    let err = compile_with_config(&make_config(true), wat).expect_err(&format!(
        "{label}: expected reference-type refusal under AN"
    ));
    let s = format!("{err:#}");
    assert!(
        s.contains("AN-encoding") && s.contains("reference type"),
        "{label}: error did not mention reference-type refusal: {s}",
    );
}

#[test]
fn refuse_externref_param_under_an() {
    let wat = r#"
        (module
            (func (export "f") (param externref) (result i32) i32.const 0))
    "#;
    assert_an_ref_refusal(wat, "externref param");
}

#[test]
fn refuse_externref_result_under_an() {
    let wat = r#"
        (module
            (func (export "f") (result externref) ref.null extern))
    "#;
    assert_an_ref_refusal(wat, "externref result");
}

#[test]
fn refuse_externref_local_under_an() {
    let wat = r#"
        (module
            (func (export "f") (result i32)
                (local externref)
                i32.const 0))
    "#;
    assert_an_ref_refusal(wat, "externref local");
}

#[test]
fn refuse_externref_global_under_an() {
    let wat = r#"
        (module
            (global $g externref (ref.null extern))
            (func (export "f") (result i32) i32.const 0))
    "#;
    assert_an_ref_refusal(wat, "externref global");
}

#[test]
fn refuse_externref_table_under_an() {
    // Non-funcref table element type: the reference-types / GC surface AN
    // does not protect. Refused even though the table itself carries no
    // encodable payload.
    let wat = r#"
        (module
            (table 1 externref)
            (func (export "f") (result i32) i32.const 0))
    "#;
    assert_an_ref_refusal(wat, "externref table");
}

// The carve-out: `funcref` tables back `call_indirect` (all dynamic dispatch,
// incl. Rust trait objects) and must keep compiling under AN. Broader
// end-to-end coverage lives in the `table_*` tests; this guards the exact
// boundary that the reference-type refusal must not cross.
#[test]
fn funcref_table_compiles_under_an() -> wasmtime::Result<()> {
    let wat = r#"
        (module
            (table 2 funcref)
            (type $t (func (result i32)))
            (func $a (result i32) i32.const 1)
            (elem (i32.const 0) $a)
            (func (export "call") (param i32) (result i32)
                (call_indirect (type $t) (local.get 0))))
    "#;
    let _ = compile_with_config(&make_config(true), wat)?;
    Ok(())
}

#[test]
fn imported_memory_compiles_under_an() -> wasmtime::Result<()> {
    // Imported (non-shared) memories are supported: the importing instance
    // mirrors stores through the owning instance's shadow via the
    // `VMMemoryImport::an_enc_base_slot` indirection. End-to-end runtime
    // coverage lives in the `imported_memory_*` tests below; this guards
    // that compilation (incl. stores/loads against the import) succeeds.
    let wat = r#"
        (module
            (import "env" "m" (memory 1))
            (func (export "poke") (param i32 i32)
                (i32.store (local.get 0) (local.get 1)))
            (func (export "peek") (param i32) (result i32)
                (i32.load (local.get 0))))
    "#;
    let _ = compile_with_config(&make_config(true), wat)?;
    Ok(())
}

// Multi-memory: the dual-buffer plumbing is per-defined-memory and exercised
// end-to-end, so compilation must succeed.
#[test]
fn multi_memory_compiles_under_an() -> wasmtime::Result<()> {
    let wat = r#"
        (module
            (memory (export "m0") 1)
            (memory (export "m1") 1)
            (func (export "f") (result i32) i32.const 0))
    "#;
    let mut config = make_config(true);
    config.wasm_multi_memory(true);
    let engine = Engine::new(&config)?;
    let _module = Module::new(&engine, wat)?;
    Ok(())
}

#[test]
fn refuse_shared_memory_under_an() {
    let wat = r#"
        (module
            (memory (export "m") 1 1 shared)
            (func (export "f") (result i32) i32.const 0))
    "#;
    let mut config = make_config(true);
    config.wasm_threads(true);
    assert_an_refusal(&config, wat, "shared memory");
}

// Atomic memory ops (threads proposal) have no shadow-update wiring, so they
// are refused when AN is on. Each test exercises a representative op and
// asserts the compile error mentions AN-encoding.

fn an_refusal_with_threads(wat: &str, label: &str) {
    let mut config = make_config(true);
    config.wasm_threads(true);
    assert_an_refusal(&config, wat, label);
}

#[test]
fn refuse_atomic_load_under_an() {
    let wat = r#"
        (module
            (memory (export "m") 1 1)
            (func (export "f") (result i32)
                i32.const 0
                i32.atomic.load))
    "#;
    an_refusal_with_threads(wat, "i32.atomic.load");
}

#[test]
fn refuse_atomic_store_under_an() {
    let wat = r#"
        (module
            (memory (export "m") 1 1)
            (func (export "f")
                i32.const 0
                i32.const 1
                i32.atomic.store))
    "#;
    an_refusal_with_threads(wat, "i32.atomic.store");
}

#[test]
fn refuse_atomic_rmw_add_under_an() {
    let wat = r#"
        (module
            (memory (export "m") 1 1)
            (func (export "f") (result i32)
                i32.const 0
                i32.const 1
                i32.atomic.rmw.add))
    "#;
    an_refusal_with_threads(wat, "i32.atomic.rmw.add");
}

#[test]
fn refuse_atomic_rmw_cmpxchg_under_an() {
    let wat = r#"
        (module
            (memory (export "m") 1 1)
            (func (export "f") (result i32)
                i32.const 0
                i32.const 1
                i32.const 2
                i32.atomic.rmw.cmpxchg))
    "#;
    an_refusal_with_threads(wat, "i32.atomic.rmw.cmpxchg");
}

#[test]
fn refuse_atomic_fence_under_an() {
    let wat = r#"
        (module
            (memory (export "m") 1 1)
            (func (export "f")
                atomic.fence))
    "#;
    an_refusal_with_threads(wat, "atomic.fence");
}

#[test]
fn refuse_memory_atomic_wait32_under_an() {
    let wat = r#"
        (module
            (memory (export "m") 1 1 shared)
            (func (export "f") (result i32)
                i32.const 0
                i32.const 1
                i64.const 0
                memory.atomic.wait32))
    "#;
    // Note: shared memory is also refused, so this would normally bounce on
    // shared. To isolate atomic-ops refusal we use a separate wat without
    // `shared`. wasmparser allows wait32 on non-shared memory at the parse
    // level even though it traps at runtime, so the atomic-op walk fires
    // before the shared-memory walk.
    let wat_nonshared = r#"
        (module
            (memory (export "m") 1 1)
            (func (export "f") (result i32)
                i32.const 0
                i32.const 1
                i64.const 0
                memory.atomic.wait32))
    "#;
    let _ = wat;
    an_refusal_with_threads(wat_nonshared, "memory.atomic.wait32");
}

#[test]
fn refuse_memory_atomic_notify_under_an() {
    let wat = r#"
        (module
            (memory (export "m") 1 1)
            (func (export "f") (result i32)
                i32.const 0
                i32.const 1
                memory.atomic.notify))
    "#;
    an_refusal_with_threads(wat, "memory.atomic.notify");
}

#[test]
fn instantiate_data_segment_under_an() -> wasmtime::Result<()> {
    // Data-segment init runs `Instance::an_encode_full_memory_from_raw` after
    // raw bytes land, so the shadow must reflect the segment content. Verify
    // by reading each byte back through a wasm `i32.load8_u`: under AN the
    // mandatory load-side check asserts the shadow slot matches raw, so a
    // divergence in the segment's shadow would trap the load.
    let wat = r#"
        (module
            (memory (export "m") 1)
            (data (i32.const 0) "Hello, AN-encoding!")
            (func (export "load_byte") (param $a i32) (result i32)
                local.get $a i32.load8_u))
    "#;
    let mut config = make_config(true);
    config.an_constant(65521);
    let engine = Engine::new(&config)?;
    let module = Module::new(&engine, wat)?;
    let mut store = Store::new(&engine, ());
    let instance = Linker::new(&engine).instantiate(&mut store, &module)?;
    let load_byte = instance.get_typed_func::<i32, i32>(&mut store, "load_byte")?;
    let expected = b"Hello, AN-encoding!";
    for (i, &want) in expected.iter().enumerate() {
        let got = load_byte.call(&mut store, i as i32)? as u8;
        assert_eq!(got, want, "data segment byte {i}");
    }
    Ok(())
}

#[test]
fn memory32_address_codeword_check_traps() -> wasmtime::Result<()> {
    const A: u64 = 7;
    let wat = r#"
        (module
            (memory (export "m") 1)
            (global (export "addr") (mut i32) (i32.const 8))
            (func (export "store32") (param i32)
                global.get 0 local.get 0 i32.store)
            (func (export "load_global") (result i32)
                global.get 0 i32.load))
    "#;
    let mut config = make_config(true);
    config.an_constant(A);
    let engine = Engine::new(&config)?;
    let module = Module::new(&engine, wat)?;
    let mut store = Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
    let store32 = instance.get_typed_func::<i32, ()>(&mut store, "store32")?;
    let load = instance.get_typed_func::<(), i32>(&mut store, "load_global")?;
    let addr = instance.get_global(&mut store, "addr").unwrap();

    store32.call(&mut store, 0x1122_3344)?;
    assert_eq!(load.call(&mut store, ())?, 0x1122_3344);

    addr.an_corrupt_i64_slot_for_test(&mut store, (A * 8 + 1) as i64);
    let err = load
        .call(&mut store, ())
        .expect_err("memory32 load with invalid encoded address should trap");
    let trap = err
        .downcast_ref::<wasmtime::Trap>()
        .unwrap_or_else(|| panic!("memory32 address trap was not a Trap: {err:?}"));
    assert_eq!(*trap, wasmtime::Trap::AnCodewordInvalid);
    Ok(())
}

#[test]
fn memory64_with_an_is_allowed_and_encoded() -> wasmtime::Result<()> {
    let wat = r#"
        (module
            (memory (export "m") i64 1)
            (func (export "store32") (param i64 i32)
                local.get 0 local.get 1 i32.store)
            (func (export "load32") (param i64) (result i32)
                local.get 0 i32.load)
            (func (export "store64") (param i64 i64)
                local.get 0 local.get 1 i64.store)
            (func (export "load64") (param i64) (result i64)
                local.get 0 i64.load))
    "#;
    let mut config = make_config(true);
    config.wasm_memory64(true);
    let engine = Engine::new(&config)?;
    let module = Module::new(&engine, wat)?;
    let mut store = Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
    let store32 = instance.get_typed_func::<(i64, i32), ()>(&mut store, "store32")?;
    let load32 = instance.get_typed_func::<i64, i32>(&mut store, "load32")?;
    let store64 = instance.get_typed_func::<(i64, i64), ()>(&mut store, "store64")?;
    let load64 = instance.get_typed_func::<i64, i64>(&mut store, "load64")?;

    for (addr, value) in [
        (0i64, 0x1122_3344u32 as i32),
        (1, 0x5566_7788u32 as i32),
        (3, 0x99aa_bbccu32 as i32),
        (13, 0xddee_ff00u32 as i32),
    ] {
        store32.call(&mut store, (addr, value))?;
        assert_eq!(load32.call(&mut store, addr)?, value, "i32 at {addr}");
    }

    for (addr, value) in [
        (32i64, 0x0123_4567_89ab_cdefu64 as i64),
        (33, -1),
        (39, i64::MIN),
        (64, i64::MAX),
    ] {
        store64.call(&mut store, (addr, value))?;
        assert_eq!(load64.call(&mut store, addr)?, value, "i64 at {addr}");
    }

    Ok(())
}

#[test]
fn memory64_address_codeword_check_traps() -> wasmtime::Result<()> {
    const A: u64 = 7;
    let wat = r#"
        (module
            (memory (export "m") i64 1)
            (global (export "addr") (mut i64) (i64.const 8))
            (func (export "store32") (param i32)
                global.get 0 local.get 0 i32.store)
            (func (export "load_global") (result i32)
                global.get 0 i32.load))
    "#;
    let mut config = make_config(true);
    config.wasm_memory64(true);
    config.an_constant(A);
    let engine = Engine::new(&config)?;
    let module = Module::new(&engine, wat)?;
    let mut store = Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
    let store32 = instance.get_typed_func::<i32, ()>(&mut store, "store32")?;
    let load = instance.get_typed_func::<(), i32>(&mut store, "load_global")?;
    let addr = instance.get_global(&mut store, "addr").unwrap();

    store32.call(&mut store, 0x1122_3344)?;
    assert_eq!(load.call(&mut store, ())?, 0x1122_3344);

    addr.an_corrupt_u128_slot_for_test(&mut store, u128::from(A) * 8 + 1);
    let err = load
        .call(&mut store, ())
        .expect_err("memory64 load with invalid encoded address should trap");
    let trap = err
        .downcast_ref::<wasmtime::Trap>()
        .unwrap_or_else(|| panic!("memory64 address trap was not a Trap: {err:?}"));
    assert_eq!(*trap, wasmtime::Trap::AnCodewordInvalid);
    Ok(())
}

// Fault-injection tests. Each one tampers with linear memory from the host
// side between instance setup and the next host-call boundary, then triggers
// a host call from wasm and asserts the cross-check raises
// `Trap::AnMemoryMismatch`. Any divergence between raw bytes and the encoded
// shadow — regardless of which side was flipped — surfaces deterministically
// at the trampoline.

/// Builds an AN-encoding instance whose wasm calls a single host function
/// that does nothing. Returns the store, the instance, the imported host
/// function, the memory, and the `f` export (taking 0 args, returning i32).
/// Each test pokes the memory between setup and `f.call(...)` and expects
/// the trap.
fn fault_injection_setup(
    a: u64,
) -> wasmtime::Result<(
    Store<()>,
    wasmtime::Instance,
    wasmtime::Memory,
    wasmtime::TypedFunc<(), i32>,
)> {
    let wat = r#"
        (module
            (import "env" "noop" (func $noop))
            (memory (export "m") 1)
            (func (export "f") (result i32)
                call $noop
                i32.const 0))
    "#;
    let mut config = make_config(true);
    config.an_constant(a);
    let engine = Engine::new(&config)?;
    let module = Module::new(&engine, wat)?;
    let mut linker = Linker::new(&engine);
    linker.func_wrap("env", "noop", |_caller: wasmtime::Caller<'_, ()>| {})?;
    let mut store = Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &module)?;
    let memory = instance
        .get_memory(&mut store, "m")
        .expect("memory export missing");
    let f = instance.get_typed_func::<(), i32>(&mut store, "f")?;
    Ok((store, instance, memory, f))
}

fn expect_an_mismatch_trap(res: wasmtime::Result<i32>, label: &str) {
    let err = res.expect_err(&format!("{label}: expected AnMemoryMismatch trap, got Ok"));
    let trap = err
        .downcast_ref::<wasmtime::Trap>()
        .unwrap_or_else(|| panic!("{label}: not a Trap: {err:?}"));
    assert_eq!(
        *trap,
        wasmtime::Trap::AnMemoryMismatch,
        "{label}: wrong trap code"
    );
}

fn expect_an_codeword_invalid_trap(res: wasmtime::Result<i32>, label: &str) {
    let err = res.expect_err(&format!("{label}: expected AnCodewordInvalid trap, got Ok"));
    let trap = err
        .downcast_ref::<wasmtime::Trap>()
        .unwrap_or_else(|| panic!("{label}: not a Trap: {err:?}"));
    assert_eq!(
        *trap,
        wasmtime::Trap::AnCodewordInvalid,
        "{label}: wrong trap code"
    );
}

/// Assert that a host `Memory::read` of the 4-byte slot at `offset` fails its
/// verify-at-use cross-check (returns `Err`).
///
/// This is the host-side counterpart of `expect_an_mismatch_trap`: under
/// verify-at-use a raw/shadow divergence is caught when the bytes are *read*,
/// not at a host-call boundary. `Memory::read` returns `MemoryAccessError`,
/// whose typed signature cannot carry the `AnMemoryMismatch` trap code, but the
/// error message is enriched to name the AN mismatch (rather than the generic
/// "out of bounds" text) so the cause is not mistaken for a bounds error.
fn expect_host_read_mismatch<T>(
    mem: &wasmtime::Memory,
    store: &mut Store<T>,
    offset: usize,
    label: &str,
) {
    let mut buf = [0u8; 4];
    let err = mem.read(&*store, offset, &mut buf).expect_err(&format!(
        "{label}: expected host read verify-at-use failure, got Ok"
    ));
    let msg = err.to_string();
    assert!(
        msg.contains("AN-encoding") && msg.to_lowercase().contains("mismatch"),
        "{label}: expected an AN mismatch message, got {msg:?}"
    );
}

/// Tampers one raw memory byte WITHOUT going through any host-write API,
/// modeling an external fault (bit flip). `Memory::data_mut` is no longer
/// suitable for fault injection: it marks the memory whole-dirty, and the
/// boundary check then (correctly) treats the change as a legitimate
/// untracked host write and resyncs the shadow instead of trapping.
fn tamper_raw_byte(
    memory: &wasmtime::Memory,
    store: &mut Store<()>,
    offset: usize,
    f: impl FnOnce(u8) -> u8,
) {
    assert!(offset < memory.data_size(&mut *store));
    let base = memory.data_ptr(&mut *store);
    // SAFETY: in-bounds (asserted above) and no outstanding borrow of the
    // memory exists — the store is only used to resolve the pointer.
    unsafe {
        let p = base.add(offset);
        p.write(f(p.read()));
    }
}

#[test]
fn fault_inject_flip_in_raw_traps() -> wasmtime::Result<()> {
    let (mut store, _instance, memory, _f) = fault_injection_setup(65521)?;
    // The whole memory was zeroed at instantiation and re-encoded into the
    // shadow, so raw[0..4] == 0 and shadow[0..8] == A·0 == 0. Flip a single
    // bit in raw to introduce a divergence. Verify-at-use: the divergence is
    // caught when the slot is read (host `Memory::read` of slot 0), not at a
    // host-call boundary.
    tamper_raw_byte(&memory, &mut store, 3, |b| b ^ 0x80);
    expect_host_read_mismatch(&memory, &mut store, 0, "raw bit flip");
    Ok(())
}

#[test]
fn fault_inject_flip_in_shadow_traps() -> wasmtime::Result<()> {
    // Symmetric to the raw-flip test: tamper the encoded shadow directly via
    // the `#[doc(hidden)]` `Memory::an_shadow_data_mut_for_test` accessor.
    // The slot is initialized to `A*0 == 0` at setup; flipping any shadow
    // byte makes `enc_slot % A != 0` (or `enc_slot / A != raw_u32`),
    // surfacing as `AnMemoryMismatch` at the next host-call cross-check.
    let (mut store, _instance, memory, _f) = fault_injection_setup(65521)?;
    let shadow = memory
        .an_shadow_data_mut_for_test(&mut store)
        .expect("shadow allocated under AN");
    // shadow[8] is shadow slot 1, i.e. raw bytes [4, 8). Reading that slot
    // surfaces the divergence (host `Memory::read` of offset 4).
    shadow[8] ^= 0x01;
    expect_host_read_mismatch(&memory, &mut store, 4, "shadow byte flip");
    Ok(())
}

#[test]
fn subword_store_checks_old_shadow_codeword() -> wasmtime::Result<()> {
    const A: u64 = 7;
    let wat = r#"
        (module
            (memory (export "m") 1)
            (func (export "store8") (param i32 i32)
                local.get 0 local.get 1 i32.store8))
    "#;
    let mut config = make_config(true);
    config.an_constant(A);
    let engine = Engine::new(&config)?;
    let module = Module::new(&engine, wat)?;
    let mut store = Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
    let memory = instance.get_memory(&mut store, "m").unwrap();
    let store8 = instance.get_typed_func::<(i32, i32), ()>(&mut store, "store8")?;

    memory
        .an_shadow_data_mut_for_test(&mut store)
        .expect("shadow allocated under AN")[0] = 1;

    let err = store8
        .call(&mut store, (1, 0x12))
        .expect_err("subword store should trap on invalid old shadow codeword");
    let trap = err
        .downcast_ref::<wasmtime::Trap>()
        .unwrap_or_else(|| panic!("subword store trap was not a Trap: {err:?}"));
    assert_eq!(*trap, wasmtime::Trap::AnCodewordInvalid);
    Ok(())
}

#[test]
#[should_panic(expected = "AnMemoryMismatch")]
fn data_mut_whole_verify_detects_pre_existing_corruption() {
    // `Memory::data_mut` hands out an opaque whole-memory mutable borrow and
    // marks the memory whole-dirty, so the next heal re-encodes the WHOLE
    // memory from raw — which would launder any pre-existing corruption.
    // Per verify-at-use the accessor cross-checks the whole memory BEFORE
    // handing out the borrow. A raw byte tampered via the untracked `data_ptr`
    // path (not `data_mut`, which would legitimately resync) must make that
    // pre-borrow check fire. The accessor is infallible (`-> &mut [u8]`), so a
    // detected mismatch can only panic.
    let (mut store, _instance, memory, _f) = fault_injection_setup(65521).unwrap();
    tamper_raw_byte(&memory, &mut store, 3, |b| b ^ 0x80);
    let _ = memory.data_mut(&mut store);
}

#[test]
fn try_data_traps_on_tamper() -> wasmtime::Result<()> {
    // `Memory::try_data` is the fallible twin of `Memory::data`: a pre-existing
    // raw/shadow divergence must surface as `Err(Trap::AnMemoryMismatch)`
    // rather than the panic `data` raises.
    let (mut store, _instance, memory, _f) = fault_injection_setup(65521)?;
    tamper_raw_byte(&memory, &mut store, 3, |b| b ^ 0x80);
    let err = memory
        .try_data(&store)
        .expect_err("try_data: expected AnMemoryMismatch, got Ok");
    let trap = err
        .downcast_ref::<wasmtime::Trap>()
        .unwrap_or_else(|| panic!("try_data: not a Trap: {err:?}"));
    assert_eq!(
        *trap,
        wasmtime::Trap::AnMemoryMismatch,
        "try_data: wrong trap"
    );
    Ok(())
}

#[test]
fn try_data_mut_traps_on_tamper() -> wasmtime::Result<()> {
    // `Memory::try_data_mut` must cross-check BEFORE marking the memory
    // whole-dirty, so a pre-existing divergence traps instead of being
    // laundered into the shadow by the next heal.
    let (mut store, _instance, memory, _f) = fault_injection_setup(65521)?;
    tamper_raw_byte(&memory, &mut store, 3, |b| b ^ 0x80);
    let err = memory
        .try_data_mut(&mut store)
        .expect_err("try_data_mut: expected AnMemoryMismatch, got Ok");
    let trap = err
        .downcast_ref::<wasmtime::Trap>()
        .unwrap_or_else(|| panic!("try_data_mut: not a Trap: {err:?}"));
    assert_eq!(
        *trap,
        wasmtime::Trap::AnMemoryMismatch,
        "try_data_mut: wrong trap"
    );
    Ok(())
}

#[test]
fn try_data_clean_passes() -> wasmtime::Result<()> {
    // With no tampering the fallible twins return Ok and expose the live bytes.
    let (mut store, _instance, memory, _f) = fault_injection_setup(65521)?;
    assert_eq!(memory.try_data(&store)?[0], 0, "clean try_data");
    assert_eq!(memory.try_data_mut(&mut store)?[0], 0, "clean try_data_mut");
    Ok(())
}

// ---------------------------------------------------------------------------
// Host-boundary `Global::get` codeword validity.
//
// An AN-encoded i32 global stores `A*v` in its 64-bit slot. The guest reads
// the encoded form directly; the host boundary (`Global::get`) decodes it. A
// corrupted slot that is not a multiple of `A` is an invalid codeword: `get`
// panics, `try_get` returns `Trap::AnCodewordInvalid`.
// ---------------------------------------------------------------------------

const GLOBAL_CODEWORD_A: u64 = 1009;

fn global_codeword_setup() -> wasmtime::Result<(Store<()>, wasmtime::Global)> {
    use wasmtime::Val;
    let mut config = Config::new();
    config.an_encoding(true);
    config.an_constant(GLOBAL_CODEWORD_A);
    let engine = Engine::new(&config)?;
    let module = Module::new(&engine, GLOBAL_EXPORT_WAT)?;
    let mut store = Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
    let g = instance.get_global(&mut store, "g").unwrap();
    g.set(&mut store, Val::I32(42))?; // stores A*42, a valid codeword
    Ok((store, g))
}

fn global_i64_codeword_setup() -> wasmtime::Result<(Store<()>, wasmtime::Global)> {
    use wasmtime::Val;
    let mut config = Config::new();
    config.an_encoding(true);
    config.an_constant(GLOBAL_CODEWORD_A);
    let engine = Engine::new(&config)?;
    let module = Module::new(&engine, GLOBAL_I64_EXPORT_WAT)?;
    let mut store = Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
    let g = instance.get_global(&mut store, "g").unwrap();
    g.set(&mut store, Val::I64(42))?;
    Ok((store, g))
}

#[test]
fn global_try_get_clean_passes() -> wasmtime::Result<()> {
    let (mut store, g) = global_codeword_setup()?;
    assert_eq!(g.try_get(&mut store)?.unwrap_i32(), 42, "clean try_get");
    Ok(())
}

#[test]
fn global_i64_try_get_clean_passes() -> wasmtime::Result<()> {
    let (mut store, g) = global_i64_codeword_setup()?;
    assert_eq!(g.try_get(&mut store)?.unwrap_i64(), 42, "clean i64 try_get");
    Ok(())
}

#[test]
fn global_try_get_invalid_codeword_traps() -> wasmtime::Result<()> {
    let (mut store, g) = global_codeword_setup()?;
    // Corrupt the slot to `A*42 + 1`, which is not a multiple of A.
    g.an_corrupt_i64_slot_for_test(&mut store, (GLOBAL_CODEWORD_A * 42 + 1) as i64);
    let err = g
        .try_get(&mut store)
        .expect_err("try_get: expected AnCodewordInvalid, got Ok");
    let trap = err
        .downcast_ref::<wasmtime::Trap>()
        .unwrap_or_else(|| panic!("try_get: not a Trap: {err:?}"));
    assert_eq!(
        *trap,
        wasmtime::Trap::AnCodewordInvalid,
        "try_get: wrong trap"
    );
    Ok(())
}

#[test]
fn global_i64_try_get_invalid_codeword_traps() -> wasmtime::Result<()> {
    let (mut store, g) = global_i64_codeword_setup()?;
    let corrupt = u128::from(GLOBAL_CODEWORD_A) * 42 + 1;
    g.an_corrupt_u128_slot_for_test(&mut store, corrupt);
    let err = g
        .try_get(&mut store)
        .expect_err("i64 try_get: expected AnCodewordInvalid, got Ok");
    let trap = err
        .downcast_ref::<wasmtime::Trap>()
        .unwrap_or_else(|| panic!("i64 try_get: not a Trap: {err:?}"));
    assert_eq!(
        *trap,
        wasmtime::Trap::AnCodewordInvalid,
        "i64 try_get: wrong trap"
    );
    Ok(())
}

#[test]
#[should_panic(expected = "AnCodewordInvalid")]
fn global_get_panics_on_invalid_codeword() {
    let (mut store, g) = global_codeword_setup().unwrap();
    g.an_corrupt_i64_slot_for_test(&mut store, (GLOBAL_CODEWORD_A * 42 + 1) as i64);
    let _ = g.get(&mut store);
}

#[test]
#[should_panic(expected = "AnCodewordInvalid")]
fn global_i64_get_panics_on_invalid_codeword() {
    let (mut store, g) = global_i64_codeword_setup().unwrap();
    let corrupt = u128::from(GLOBAL_CODEWORD_A) * 42 + 1;
    g.an_corrupt_u128_slot_for_test(&mut store, corrupt);
    let _ = g.get(&mut store);
}

#[test]
fn c_api_trap_header_has_an_trap_codes() {
    let trap_h = include_str!("../../crates/c-api/include/wasmtime/trap.h");
    assert!(trap_h.contains("WASMTIME_TRAP_CODE_AN_MEMORY_MISMATCH = 48"));
    assert!(trap_h.contains("WASMTIME_TRAP_CODE_AN_CODEWORD_INVALID = 49"));
    assert!(trap_h.contains("WASMTIME_TRAP_CODE_AN_I64_WIDEN_OVERFLOW = 50"));
}

#[test]
fn fault_inject_various_an_constants() -> wasmtime::Result<()> {
    // The cross-check is independent of A, but exercise a few values to
    // confirm both the decode (slot / A) and the modular check (slot % A)
    // produce a trap consistently.
    for &a in &[1u64, 7, 1009, 65521, 16_777_215] {
        let (mut store, _instance, memory, _f) = fault_injection_setup(a)?;
        tamper_raw_byte(&memory, &mut store, 16, |_| 0x55);
        expect_host_read_mismatch(&memory, &mut store, 16, &format!("raw poke with A={a}"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// `memory.grow` shadow-maintenance regression tests.
//
// Growing an AN memory must (1) preserve the existing encoded shadow and (2)
// NOT re-encode the shadow from raw. Re-encoding on every grow was an
// O(memory-size) operation that committed the entire 2x shadow — for a
// multi-GiB memory (the `big-strings.wast` component test) that turned an
// otherwise lazy/VM-based grow into a multi-second, multi-GiB spin. It was
// also a fault-detection hole: re-encoding from raw silently absorbs any
// raw/shadow divergence the cross-check exists to catch.
// ---------------------------------------------------------------------------

const GROW_WAT: &str = r#"
    (module
        (import "env" "noop" (func $noop))
        (memory (export "m") 1)
        (func (export "grow") (param i32) (result i32)
            (memory.grow (local.get 0)))
        (func (export "store") (param $addr i32) (param $val i32)
            (i32.store (local.get $addr) (local.get $val)))
        (func (export "load") (param $addr i32) (result i32)
            (i32.load (local.get $addr)))
        (func (export "f") (result i32)
            call $noop
            i32.const 0))
"#;

fn grow_setup(
    a: u64,
) -> wasmtime::Result<(
    Store<()>,
    wasmtime::Memory,
    wasmtime::TypedFunc<i32, i32>,
    wasmtime::TypedFunc<(i32, i32), ()>,
    wasmtime::TypedFunc<i32, i32>,
    wasmtime::TypedFunc<(), i32>,
)> {
    let mut config = make_config(true);
    config.an_constant(a);
    let engine = Engine::new(&config)?;
    let module = Module::new(&engine, GROW_WAT)?;
    let mut linker = Linker::new(&engine);
    linker.func_wrap("env", "noop", |_caller: wasmtime::Caller<'_, ()>| {})?;
    let mut store = Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &module)?;
    let memory = instance.get_memory(&mut store, "m").expect("memory export");
    let grow = instance.get_typed_func::<i32, i32>(&mut store, "grow")?;
    let st = instance.get_typed_func::<(i32, i32), ()>(&mut store, "store")?;
    let ld = instance.get_typed_func::<i32, i32>(&mut store, "load")?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "f")?;
    Ok((store, memory, grow, st, ld, f))
}

// Sharp guard: a raw/shadow divergence introduced *before* a grow must survive
// it. If `memory.grow` re-encoded the shadow from raw it would absorb the
// corruption and the host-side cross-check would (wrongly) pass.
#[test]
fn grow_does_not_resync_shadow_from_raw() -> wasmtime::Result<()> {
    let (mut store, memory, grow, _st, _ld, _f) = grow_setup(65521)?;
    // raw[0..4] == 0 and shadow[0..8] == A*0 == 0 after instantiation. Flip a
    // raw bit (untracked, modeling a fault) so raw and shadow disagree.
    tamper_raw_byte(&memory, &mut store, 3, |b| b ^ 0x80);
    // Grow a page. The divergence must survive (shadow copied forward, not
    // re-encoded from the corrupted raw). Aligned guest i32 loads intentionally
    // use shadow as their sole source of truth, so verify the raw side through
    // the host read path.
    grow.call(&mut store, 1)?;
    expect_host_read_mismatch(&memory, &mut store, 0, "raw corruption survives grow");
    Ok(())
}

// Copy-forward correctness across repeated grows: previously written encoded
// data stays intact and the shadow stays consistent with raw (cross-check
// passes) after each grow.
#[test]
fn grow_preserves_shadow_across_repeated_grows() -> wasmtime::Result<()> {
    let (mut store, _memory, grow, st, ld, _f) = grow_setup(65521)?;
    // Sentinel in the last full slot of page 0.
    let sentinel = 0x1234_5678u32 as i32;
    st.call(&mut store, (65532, sentinel))?;
    // The `ld` read-backs below verify shadow/raw consistency at the sentinel
    // slot via the mandatory load-side check (no host-boundary cross-check any
    // more). A divergence introduced by a grow would trap the load.
    ld.call(&mut store, 65532)?; // shadow consistent before growing
    for delta in [1, 7, 64] {
        let prev_pages = grow.call(&mut store, delta)?;
        assert!(prev_pages > 0, "grow({delta}) should succeed");
        // Old data preserved through the grow (and the load verifies the slot's
        // shadow still matches raw after copy-forward).
        assert_eq!(
            ld.call(&mut store, 65532)?,
            sentinel,
            "sentinel lost after grow({delta})"
        );
    }
    Ok(())
}

// Unaligned `i32.store` and cross-slot `i32.store16`. The
// wat below stores at every byte offset in `0..8`, then triggers a host call
// so the cross-check runs against the shadow. If the unaligned path ever
// leaves the shadow inconsistent, the host-call cross-check raises
// `Trap::AnMemoryMismatch` and the test fails.

const UNALIGNED_WAT: &str = r#"
    (module
        (import "env" "noop" (func $noop))
        (memory (export "m") 1)
        (func (export "store_i32") (param $addr i32) (param $val i32)
            local.get $addr local.get $val i32.store
            call $noop)
        (func (export "store_i32_8") (param $addr i32) (param $val i32)
            local.get $addr local.get $val i32.store8
            call $noop)
        (func (export "store_i32_16") (param $addr i32) (param $val i32)
            local.get $addr local.get $val i32.store16
            call $noop)
        (func (export "load_i32") (param $addr i32) (result i32)
            local.get $addr i32.load)
        (func (export "load_i32_8") (param $addr i32) (result i32)
            local.get $addr i32.load8_u)
        (func (export "load_i32_16") (param $addr i32) (result i32)
            local.get $addr i32.load16_u))
"#;

fn unaligned_setup(a: u64) -> wasmtime::Result<(Store<()>, wasmtime::Instance)> {
    let mut config = make_config(true);
    config.an_constant(a);
    let engine = Engine::new(&config)?;
    let module = Module::new(&engine, UNALIGNED_WAT)?;
    let mut linker = Linker::new(&engine);
    linker.func_wrap("env", "noop", |_caller: wasmtime::Caller<'_, ()>| {})?;
    let mut store = Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &module)?;
    Ok((store, instance))
}

#[test]
fn unaligned_i32_store_every_offset() -> wasmtime::Result<()> {
    // For each address `a` in 16..24, store an i32 then load it back via
    // four byte loads to verify the raw bytes are correct, and trigger a
    // host call so the cross-check confirms the shadow matches raw. The base
    // is slot-aligned and non-zero, so `a % 4` still walks every byte position
    // (incl. the cross-slot byte_pos==3) while avoiding address 0.
    let (mut store, instance) = unaligned_setup(65521)?;
    let store_i32 = instance.get_typed_func::<(i32, i32), ()>(&mut store, "store_i32")?;
    let load_i32_8 = instance.get_typed_func::<i32, i32>(&mut store, "load_i32_8")?;

    let value: i32 = 0x12_34_56_78;
    for a in 16i32..24 {
        store_i32.call(&mut store, (a, value))?;
        for i in 0..4 {
            let got = load_i32_8.call(&mut store, a + i)?;
            let expected = ((value as u32) >> (8 * i)) & 0xff;
            assert_eq!(
                got as u32, expected,
                "unaligned i32.store at addr {a} byte {i}",
            );
        }
    }
    Ok(())
}

#[test]
fn cross_slot_i32_store16_every_offset() -> wasmtime::Result<()> {
    // `i32.store16` at byte_pos == 3 spans two shadow slots. Walk addresses
    // 16..24 (slot-aligned, non-zero base) so `a % 4` exercises both in-slot
    // (byte_pos 0,1,2) and cross-slot (byte_pos 3, addresses 19 and 23) cases.
    let (mut store, instance) = unaligned_setup(65521)?;
    let store_i32_16 = instance.get_typed_func::<(i32, i32), ()>(&mut store, "store_i32_16")?;
    let load_i32_8 = instance.get_typed_func::<i32, i32>(&mut store, "load_i32_8")?;

    let value: i32 = 0xab_cd; // low half-word
    for a in 16i32..24 {
        store_i32_16.call(&mut store, (a, value))?;
        for i in 0..2 {
            let got = load_i32_8.call(&mut store, a + i)?;
            let expected = ((value as u32) >> (8 * i)) & 0xff;
            assert_eq!(got as u32, expected, "i32.store16 at addr {a} byte {i}",);
        }
    }
    Ok(())
}

#[test]
fn unaligned_store_then_aligned_store_same_slot() -> wasmtime::Result<()> {
    // After an unaligned store touches a slot via byte-RMW decomposition,
    // a subsequent aligned store to the same slot must produce the right
    // final encoded value (verifies the byte-RMW path leaves the
    // `A * u32` invariant intact for downstream reads/stores).
    let (mut store, instance) = unaligned_setup(65521)?;
    let store_i32 = instance.get_typed_func::<(i32, i32), ()>(&mut store, "store_i32")?;
    let load_i32 = instance.get_typed_func::<i32, i32>(&mut store, "load_i32")?;

    // Work on the slot pair at bytes [16,20)/[20,24) — non-zero addresses
    // throughout so a missing address decode can't pass on `A*0 == 0`.
    // Write 0xAAAABBBB at addr 17 (unaligned). Bytes:
    //   raw[17]=BB raw[18]=BB raw[19]=AA raw[20]=AA
    // Slot [16,20) ends at byte 19; slot [20,24) starts at byte 20.
    store_i32.call(&mut store, (17, 0xAAAA_BBBBu32 as i32))?;

    // Now write 0x11223344 at addr 16 (aligned).
    //   raw[16]=44 raw[17]=33 raw[18]=22 raw[19]=11
    // Slot [16,20) becomes 0x11223344.
    store_i32.call(&mut store, (16, 0x1122_3344))?;

    // Aligned load of slot [16,20) should see 0x11223344.
    let v0 = load_i32.call(&mut store, 16)?;
    assert_eq!(v0 as u32, 0x1122_3344, "aligned slot after overwrite");

    // Aligned load of the next slot should still see the AA byte left by the
    // first store: raw[20] = AA, raw[21..24] = 0.
    let v4 = load_i32.call(&mut store, 20)?;
    assert_eq!(v4 as u32, 0x0000_00AA, "next slot high byte preserved");
    Ok(())
}

// Bulk memory ops must keep the encoded shadow in sync
// with raw bytes so the host-boundary cross-check does not trap. Each test
// runs a bulk op, then a host call, and asserts no trap (clean cross-check)
// plus the visible content via byte loads.

const BULK_WAT: &str = r#"
    (module
        (import "env" "noop" (func $noop))
        (memory (export "m") 1)
        (data (i32.const 200) "DATAseg")
        (func (export "fill") (param $dst i32) (param $v i32) (param $len i32)
            local.get $dst local.get $v local.get $len memory.fill
            call $noop)
        (func (export "copy") (param $dst i32) (param $src i32) (param $len i32)
            local.get $dst local.get $src local.get $len memory.copy
            call $noop)
        (func (export "init") (param $dst i32) (param $src i32) (param $len i32)
            local.get $dst local.get $src local.get $len (memory.init 0)
            call $noop)
        (func (export "grow") (param $delta i32) (result i32)
            local.get $delta memory.grow
            call $noop)
        (func (export "size") (result i32)
            memory.size)
        (func (export "store_byte") (param $a i32) (param $v i32)
            local.get $a local.get $v i32.store8
            call $noop)
        (func (export "load_byte") (param $a i32) (result i32)
            local.get $a i32.load8_u)
        (func (export "load_i32") (param $a i32) (result i32)
            local.get $a i32.load))
"#;

fn bulk_setup(a: u64) -> wasmtime::Result<(Store<()>, wasmtime::Instance)> {
    let mut config = make_config(true);
    config.an_constant(a);
    let engine = Engine::new(&config)?;
    let module = Module::new(&engine, BULK_WAT)?;
    let mut linker = Linker::new(&engine);
    linker.func_wrap("env", "noop", |_caller: wasmtime::Caller<'_, ()>| {})?;
    let mut store = Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &module)?;
    Ok((store, instance))
}

#[test]
fn bulk_wat_compiles_without_an() -> wasmtime::Result<()> {
    let config = make_config(false);
    let engine = Engine::new(&config)?;
    let _module = Module::new(&engine, BULK_WAT)?;
    Ok(())
}

#[test]
fn bulk_wat_compiles_with_an() -> wasmtime::Result<()> {
    let config = make_config(true);
    let engine = Engine::new(&config)?;
    let _module = Module::new(&engine, BULK_WAT)?;
    Ok(())
}

#[test]
fn bulk_memory_fill_keeps_shadow_consistent() -> wasmtime::Result<()> {
    let (mut store, instance) = bulk_setup(65521)?;
    let fill = instance.get_typed_func::<(i32, i32, i32), ()>(&mut store, "fill")?;
    let load_byte = instance.get_typed_func::<i32, i32>(&mut store, "load_byte")?;

    // Cover aligned fill, unaligned fill that crosses multiple slots, and a
    // non-zero fill byte. Every address operand is non-zero so a missing
    // address decode can't hide behind `A*0 == 0`. Each `fill` call ends with
    // a host call so the cross-check fires.
    fill.call(&mut store, (16, 0xAB, 16))?;
    for i in 16..32 {
        assert_eq!(
            load_byte.call(&mut store, i)? as u32,
            0xAB,
            "fill1 byte {i}"
        );
    }
    fill.call(&mut store, (19, 0xCD, 10))?; // straddles slot boundary
    for i in 16..19 {
        assert_eq!(
            load_byte.call(&mut store, i)? as u32,
            0xAB,
            "fill2 untouched {i}"
        );
    }
    for i in 19..29 {
        assert_eq!(
            load_byte.call(&mut store, i)? as u32,
            0xCD,
            "fill2 byte {i}"
        );
    }
    for i in 29..32 {
        assert_eq!(
            load_byte.call(&mut store, i)? as u32,
            0xAB,
            "fill2 untouched {i}"
        );
    }
    Ok(())
}

#[test]
fn bulk_memory_copy_keeps_shadow_consistent() -> wasmtime::Result<()> {
    let (mut store, instance) = bulk_setup(65521)?;
    let fill = instance.get_typed_func::<(i32, i32, i32), ()>(&mut store, "fill")?;
    let copy = instance.get_typed_func::<(i32, i32, i32), ()>(&mut store, "copy")?;
    let load_byte = instance.get_typed_func::<i32, i32>(&mut store, "load_byte")?;

    // Pre-fill source region, copy to disjoint dst, verify. Every address
    // operand is non-zero so a missing src/dst decode can't pass on
    // `A*0 == 0`.
    fill.call(&mut store, (16, 0x11, 8))?;
    copy.call(&mut store, (64, 16, 8))?;
    for i in 0..8 {
        assert_eq!(
            load_byte.call(&mut store, 64 + i)? as u32,
            0x11,
            "copy byte {i}"
        );
    }

    // Overlapping copy. Wasm `memory.copy` is `memmove`-safe: each
    // `dst[i] = src[i]` reads the *pre-copy* source byte. With pre-copy
    // raw[100..108] = [22 22 22 22 33 33 33 33] and `copy(dst=102, src=100,
    // len=8)`, the result is raw[102..110] = pre-copy raw[100..108]. Bytes
    // 100,101 are outside the destination range and stay 0x22.
    fill.call(&mut store, (100, 0x22, 4))?;
    fill.call(&mut store, (104, 0x33, 4))?;
    copy.call(&mut store, (102, 100, 8))?;
    let expected = [
        0x22, 0x22, // raw[100..102] untouched
        0x22, 0x22, 0x22, 0x22, // raw[102..106] = pre-copy raw[100..104]
        0x33, 0x33, 0x33, 0x33, // raw[106..110] = pre-copy raw[104..108]
    ];
    for (i, &want) in expected.iter().enumerate() {
        assert_eq!(
            load_byte.call(&mut store, 100 + i as i32)? as u32,
            want as u32,
            "overlap byte {i}",
        );
    }
    Ok(())
}

#[test]
fn memory_copy_source_tamper_traps() -> wasmtime::Result<()> {
    // `memory.copy` reads its SOURCE range out of guest memory. Under
    // verify-at-use that read must be cross-checked against the encoded shadow
    // *before* the bytes are copied (and before the destination shadow is
    // re-encoded from them). Otherwise a raw/shadow divergence in the source is
    // (a) copied to the destination and (b) laundered into a valid destination
    // codeword by the post-copy re-encode, so no later load-side check can ever
    // catch it.
    let (mut store, instance) = bulk_setup(65521)?;
    let fill = instance.get_typed_func::<(i32, i32, i32), ()>(&mut store, "fill")?;
    let copy = instance.get_typed_func::<(i32, i32, i32), ()>(&mut store, "copy")?;
    let memory = instance
        .get_memory(&mut store, "m")
        .expect("memory export missing");

    // Establish a consistent source region: raw[16..24] = 0x11, shadow in sync.
    fill.call(&mut store, (16, 0x11, 8))?;

    // Fault: flip a byte in the source region via the untracked `data_ptr`
    // path (NOT `data_mut`, which would legitimately resync). raw[18] now
    // disagrees with shadow slot 4 (bytes [16,20)).
    tamper_raw_byte(&memory, &mut store, 18, |b| b ^ 0x40);

    // Copy the tampered source to a disjoint destination. The source
    // cross-check must trap before the copy happens.
    let res = copy.call(&mut store, (64, 16, 8)).map(|()| 0);
    expect_an_mismatch_trap(res, "memory.copy tampered source");
    Ok(())
}

#[test]
fn active_data_segment_keeps_shadow_consistent() -> wasmtime::Result<()> {
    // Active data segment in `BULK_WAT` places "DATAseg" at addr 200, laid
    // down at instantiation. Verify the bytes round-trip through wasm-side
    // loads, and that the segment's shadow agrees with raw at the host
    // boundary.
    let (mut store, instance) = bulk_setup(65521)?;
    let load_byte = instance.get_typed_func::<i32, i32>(&mut store, "load_byte")?;

    // Each `load_byte` runs the mandatory load-side check, so reading the
    // segment back verifies its shadow matches raw — a divergence would trap
    // the load.
    let expected = b"DATAseg";
    for (i, &want) in expected.iter().enumerate() {
        let got = load_byte.call(&mut store, 200 + i as i32)? as u8;
        assert_eq!(got, want, "active data segment byte {i}");
    }
    Ok(())
}

// Passive data segment + explicit `memory.init` under AN. The active variant
// above is exercised by `BULK_WAT`; this one separately confirms the
// `memory.init` libcall path drives `Instance::an_encode_range_from_raw` and
// the resulting shadow agrees with raw via the host-boundary cross-check.
const PASSIVE_INIT_WAT: &str = r#"
    (module
        (import "env" "noop" (func $noop))
        (memory (export "m") 1)
        (data $d "PASSIVE")
        (func (export "do_init") (param $dst i32) (param $src i32) (param $len i32)
            local.get $dst local.get $src local.get $len memory.init $d
            call $noop)
        (func (export "load_byte") (param $a i32) (result i32)
            local.get $a i32.load8_u))
"#;

#[test]
fn passive_memory_init_keeps_shadow_consistent() -> wasmtime::Result<()> {
    let mut config = make_config(true);
    config.an_constant(65521);
    let engine = Engine::new(&config)?;
    let module = Module::new(&engine, PASSIVE_INIT_WAT)?;
    let mut linker = Linker::new(&engine);
    linker.func_wrap("env", "noop", |_caller: wasmtime::Caller<'_, ()>| {})?;
    let mut store = Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &module)?;
    let do_init = instance.get_typed_func::<(i32, i32, i32), ()>(&mut store, "do_init")?;
    let load_byte = instance.get_typed_func::<i32, i32>(&mut store, "load_byte")?;

    // Cover three placements: aligned (slot start), unaligned (mid-slot), and
    // straddling a slot boundary. dst and src are non-zero throughout so a
    // missing decode of either can't hide behind `A*0 == 0`; src=1 also proves
    // the segment offset was decoded (it selects "ASSIVE", not "PASSIVE").
    // Each call ends in a host-boundary cross-check via `call $noop`.
    let expected = b"ASSIVE";
    for &dst in &[8i32, 5, 13] {
        do_init.call(&mut store, (dst, 1, expected.len() as i32))?;
        for (i, &want) in expected.iter().enumerate() {
            let got = load_byte.call(&mut store, dst + i as i32)? as u8;
            assert_eq!(got, want, "passive init dst={dst} byte {i}");
        }
    }
    Ok(())
}

#[test]
fn bulk_memory_grow_keeps_shadow_consistent() -> wasmtime::Result<()> {
    let (mut store, instance) = bulk_setup(65521)?;
    let grow = instance.get_typed_func::<i32, i32>(&mut store, "grow")?;
    let size = instance.get_typed_func::<(), i32>(&mut store, "size")?;
    let store_byte = instance.get_typed_func::<(i32, i32), ()>(&mut store, "store_byte")?;
    let load_byte = instance.get_typed_func::<i32, i32>(&mut store, "load_byte")?;

    // Write a sentinel before growing — must survive grow + cross-check.
    store_byte.call(&mut store, (1024, 0x77))?;
    assert_eq!(load_byte.call(&mut store, 1024)? as u32, 0x77);

    // Grow by 1 page (64 KiB). Old content preserved, new pages zeroed.
    let old_pages = grow.call(&mut store, 1)?;
    assert_eq!(old_pages, 1);
    assert_eq!(size.call(&mut store, ())?, 2);

    // Sentinel still readable.
    assert_eq!(load_byte.call(&mut store, 1024)? as u32, 0x77);

    // New page (at offset 65536) reads back as zero.
    assert_eq!(load_byte.call(&mut store, 65_536 + 100)? as u32, 0);

    // Write/read on the new page exercises the freshly grown shadow.
    store_byte.call(&mut store, (65_536 + 100, 0x99))?;
    assert_eq!(load_byte.call(&mut store, 65_536 + 100)? as u32, 0x99);
    Ok(())
}

#[test]
fn bulk_memory_with_various_an_constants() -> wasmtime::Result<()> {
    // Re-run a small bulk-op sequence with several A values to confirm the
    // shadow range encoder reads A from tunables consistently.
    for &a in &[1u64, 7, 1009, 65521, 16_777_215] {
        let (mut store, instance) = bulk_setup(a)?;
        let fill = instance.get_typed_func::<(i32, i32, i32), ()>(&mut store, "fill")?;
        let load_byte = instance.get_typed_func::<i32, i32>(&mut store, "load_byte")?;
        fill.call(&mut store, (5, 0xEF, 13))?;
        for i in 5..18 {
            assert_eq!(
                load_byte.call(&mut store, i)? as u32,
                0xEF,
                "A={a} byte {i}",
            );
        }
    }
    Ok(())
}

// Table-op AN audit. Table ops take i32 index/length operands the
// same way bulk-memory ops do; under AN those operands arrive on the value
// stack as encoded `I64` (`A*v`) and must be decoded before flowing into the
// builtin helpers. Operators that return an i32 (`table.grow`, `table.size`)
// must re-encode the result before pushing.
//
// Each test compiles a wat module that exercises a table op under AN-on,
// then runs it and asserts the visible behavior. Without the i32 decode the
// underlying cranelift cast (`uextend.i64`) panics on an `I64` input.

const TABLE_WAT: &str = r#"
    (module
        (import "env" "noop" (func $noop))
        (table $t 4 funcref)
        (func $f0 (result i32) i32.const 100)
        (func $f1 (result i32) i32.const 200)
        (func $f2 (result i32) i32.const 300)
        (elem (i32.const 0) $f0 $f1 $f2)
        (func (export "size") (result i32) table.size $t)
        (func (export "grow") (param $delta i32) (result i32)
            ref.null func local.get $delta table.grow $t)
        (func (export "fill") (param $dst i32) (param $len i32)
            local.get $dst ref.func $f2 local.get $len table.fill $t)
        (func (export "copy") (param $dst i32) (param $src i32) (param $len i32)
            local.get $dst local.get $src local.get $len table.copy $t $t)
        (func (export "call_idx") (param $i i32) (result i32)
            local.get $i call_indirect $t (result i32))
        (func (export "trigger_host")
            call $noop))
"#;

fn table_setup(an_on: bool) -> wasmtime::Result<(Store<()>, wasmtime::Instance)> {
    let mut config = make_config(an_on);
    if an_on {
        config.an_constant(65521);
    }
    let engine = Engine::new(&config)?;
    let module = Module::new(&engine, TABLE_WAT)?;
    let mut linker = Linker::new(&engine);
    linker.func_wrap("env", "noop", |_caller: wasmtime::Caller<'_, ()>| {})?;
    let mut store = Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &module)?;
    Ok((store, instance))
}

#[test]
fn table_size_under_an() -> wasmtime::Result<()> {
    let (mut store, instance) = table_setup(true)?;
    let size = instance.get_typed_func::<(), i32>(&mut store, "size")?;
    assert_eq!(size.call(&mut store, ())?, 4);
    Ok(())
}

#[test]
fn table_grow_under_an() -> wasmtime::Result<()> {
    let (mut store, instance) = table_setup(true)?;
    let grow = instance.get_typed_func::<i32, i32>(&mut store, "grow")?;
    let size = instance.get_typed_func::<(), i32>(&mut store, "size")?;
    // Grow by 2 from initial size 4. Result is previous size (4).
    assert_eq!(grow.call(&mut store, 2)?, 4);
    assert_eq!(size.call(&mut store, ())?, 6);
    Ok(())
}

#[test]
fn table_fill_under_an() -> wasmtime::Result<()> {
    let (mut store, instance) = table_setup(true)?;
    let fill = instance.get_typed_func::<(i32, i32), ()>(&mut store, "fill")?;
    let call_idx = instance.get_typed_func::<i32, i32>(&mut store, "call_idx")?;
    // Fill slots [2, 4) with $f2. Non-zero `dst`/`len` so a missing decode of
    // either operand can't pass via `A*0 == 0`. Calling the filled slots back
    // confirms the fill landed where requested, not merely that it didn't trap.
    fill.call(&mut store, (2, 2))?;
    assert_eq!(call_idx.call(&mut store, 2)?, 300);
    assert_eq!(call_idx.call(&mut store, 3)?, 300);
    Ok(())
}

#[test]
fn table_copy_under_an() -> wasmtime::Result<()> {
    let (mut store, instance) = table_setup(true)?;
    let copy = instance.get_typed_func::<(i32, i32, i32), ()>(&mut store, "copy")?;
    let call_idx = instance.get_typed_func::<i32, i32>(&mut store, "call_idx")?;
    // Copy $f1 from slot 1 into the (initially null) slot 3. All three operands
    // are non-zero so a missing `dst`/`src`/`len` decode can't slip through on
    // `A*0 == 0`. Asserting `call_idx(3) == 200` proves `src` selected slot 1
    // ($f1), not slot 0 ($f0 -> 100), and that the copy reached slot 3.
    copy.call(&mut store, (3, 1, 1))?;
    assert_eq!(call_idx.call(&mut store, 3)?, 200);
    Ok(())
}

#[test]
fn call_indirect_under_an() -> wasmtime::Result<()> {
    let (mut store, instance) = table_setup(true)?;
    let call_idx = instance.get_typed_func::<i32, i32>(&mut store, "call_idx")?;
    // Indices are deliberately non-zero: at index 0 a missing index decode is
    // invisible (`A*0 == 0`). Distinct results per index prove the encoded
    // index was decoded before the table dispatch.
    assert_eq!(call_idx.call(&mut store, 1)?, 200);
    assert_eq!(call_idx.call(&mut store, 2)?, 300);
    Ok(())
}

#[test]
fn table_ops_match_without_an() -> wasmtime::Result<()> {
    // Sanity counterpart: same wat without AN must produce the same observable
    // outcomes. Confirms the AN-on path matches semantics, not just compiles.
    let (mut store, instance) = table_setup(false)?;
    let size = instance.get_typed_func::<(), i32>(&mut store, "size")?;
    let grow = instance.get_typed_func::<i32, i32>(&mut store, "grow")?;
    let call_idx = instance.get_typed_func::<i32, i32>(&mut store, "call_idx")?;
    assert_eq!(size.call(&mut store, ())?, 4);
    assert_eq!(grow.call(&mut store, 2)?, 4);
    assert_eq!(size.call(&mut store, ())?, 6);
    assert_eq!(call_idx.call(&mut store, 1)?, 200);
    assert_eq!(call_idx.call(&mut store, 2)?, 300);
    Ok(())
}

// table.init + elem.drop with a passive elem segment.
const TABLE_INIT_WAT: &str = r#"
    (module
        (table $t 4 funcref)
        (func $g0 (result i32) i32.const 11)
        (func $g1 (result i32) i32.const 22)
        (func $g2 (result i32) i32.const 33)
        (elem $e func $g0 $g1 $g2)
        (func (export "init") (param $dst i32) (param $src i32) (param $len i32)
            local.get $dst local.get $src local.get $len table.init $t $e)
        (func (export "drop_elem") elem.drop $e)
        (func (export "call_at") (param $i i32) (result i32)
            local.get $i call_indirect $t (result i32)))
"#;

#[test]
fn table_init_under_an() -> wasmtime::Result<()> {
    let mut config = make_config(true);
    config.an_constant(65521);
    let engine = Engine::new(&config)?;
    let module = Module::new(&engine, TABLE_INIT_WAT)?;
    let mut store = Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
    let init = instance.get_typed_func::<(i32, i32, i32), ()>(&mut store, "init")?;
    let call_at = instance.get_typed_func::<i32, i32>(&mut store, "call_at")?;
    // Init table[1..3] from the passive segment starting at src=1 ($g1, $g2).
    // dst/src/len are all non-zero, so a missing decode of any of them can't
    // hide behind `A*0 == 0`. The asserted values prove src=1 selected $g1/$g2
    // (not $g0 -> 11) and that dst placed them at slots 1 and 2.
    init.call(&mut store, (1, 1, 2))?;
    assert_eq!(call_at.call(&mut store, 1)?, 22);
    assert_eq!(call_at.call(&mut store, 2)?, 33);
    Ok(())
}

#[test]
fn fault_inject_clean_run_passes() -> wasmtime::Result<()> {
    // Sanity counterpart: a clean AN program that makes a host call must run
    // without any spurious trap and return 0. Distinguishes "AN traps on every
    // host call" from "AN traps only on a real divergence (caught on read)".
    let (mut store, _instance, _memory, f) = fault_injection_setup(65521)?;
    let r = f.call(&mut store, ())?;
    assert_eq!(r, 0, "wasm returned non-zero from a clean run");
    Ok(())
}

// Multi-memory under AN-encoding. The dual-buffer plumbing is
// per-defined-memory: each memory gets its own encoded shadow allocated in
// `Instance::set_an_enc_shadows`, and the host-call boundary cross-check
// walks `0..num_defined_memories`. Stores route through `memarg.memory` so
// the cranelift codegen already targets the right shadow. These tests
// verify the end-to-end behavior:
//   1. A wat with two memories instantiates and stores into each.
//   2. Host-call boundary cross-check passes (clean run).
//   3. Tampering either memory's raw bytes triggers `AnMemoryMismatch`.

const MULTI_MEM_WAT: &str = r#"
    (module
        (import "env" "noop" (func $noop))
        (memory $m0 (export "m0") 1)
        (memory $m1 (export "m1") 1)
        (func (export "store_m0") (param $a i32) (param $v i32)
            local.get $a local.get $v i32.store $m0
            call $noop)
        (func (export "store_m1") (param $a i32) (param $v i32)
            local.get $a local.get $v i32.store $m1
            call $noop)
        (func (export "load_m0") (param $a i32) (result i32)
            local.get $a i32.load $m0)
        (func (export "load_m1") (param $a i32) (result i32)
            local.get $a i32.load $m1)
        (func (export "trigger_host") (result i32)
            call $noop
            i32.const 0))
"#;

fn multi_memory_setup(a: u64) -> wasmtime::Result<(Store<()>, wasmtime::Instance)> {
    let mut config = make_config(true);
    config.wasm_multi_memory(true);
    config.an_constant(a);
    let engine = Engine::new(&config)?;
    let module = Module::new(&engine, MULTI_MEM_WAT)?;
    let mut linker = Linker::new(&engine);
    linker.func_wrap("env", "noop", |_caller: wasmtime::Caller<'_, ()>| {})?;
    let mut store = Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &module)?;
    Ok((store, instance))
}

#[test]
fn multi_memory_stores_keep_shadows_consistent() -> wasmtime::Result<()> {
    // Aligned stores into each defined memory should round-trip through the
    // raw buffers and pass the per-memory cross-check at the host-call
    // trampoline. If either shadow drifted, the trailing `call $noop` would
    // raise `AnMemoryMismatch`.
    let (mut store, instance) = multi_memory_setup(65521)?;
    let store_m0 = instance.get_typed_func::<(i32, i32), ()>(&mut store, "store_m0")?;
    let store_m1 = instance.get_typed_func::<(i32, i32), ()>(&mut store, "store_m1")?;
    let load_m0 = instance.get_typed_func::<i32, i32>(&mut store, "load_m0")?;
    let load_m1 = instance.get_typed_func::<i32, i32>(&mut store, "load_m1")?;

    // Non-zero addresses throughout so a missing address decode can't pass on
    // `A*0 == 0`; distinct values per (memory, address) prove both the address
    // decode and the per-memory routing.
    store_m0.call(&mut store, (8, 0x1111_2222u32 as i32))?;
    store_m1.call(&mut store, (8, 0x3333_4444u32 as i32))?;
    store_m0.call(&mut store, (16, 0x5555_6666u32 as i32))?;
    store_m1.call(&mut store, (16, 0x7777_8888u32 as i32))?;

    assert_eq!(load_m0.call(&mut store, 8)? as u32, 0x1111_2222);
    assert_eq!(load_m1.call(&mut store, 8)? as u32, 0x3333_4444);
    assert_eq!(load_m0.call(&mut store, 16)? as u32, 0x5555_6666);
    assert_eq!(load_m1.call(&mut store, 16)? as u32, 0x7777_8888);
    Ok(())
}

#[test]
fn multi_memory_tamper_mem0_traps() -> wasmtime::Result<()> {
    // Flipping a bit in defined memory 0 raw bytes diverges its shadow.
    // The host-call cross-check iterates all defined memories and must
    // report the mismatch.
    let (mut store, instance) = multi_memory_setup(65521)?;
    let m0 = instance
        .get_memory(&mut store, "m0")
        .expect("memory m0 export missing");
    tamper_raw_byte(&m0, &mut store, 3, |b| b ^ 0x80);
    // Verify-at-use: a host read of the tampered slot in m0 detects the
    // divergence (each defined memory is verified independently on read).
    expect_host_read_mismatch(&m0, &mut store, 0, "m0 raw bit flip");
    Ok(())
}

#[test]
fn multi_memory_tamper_mem1_traps() -> wasmtime::Result<()> {
    // Symmetric to the m0 test: divergence in memory 1 must also surface.
    // Confirms the cross-check loop visits every defined memory, not just
    // index 0.
    let (mut store, instance) = multi_memory_setup(65521)?;
    let m1 = instance
        .get_memory(&mut store, "m1")
        .expect("memory m1 export missing");
    // Tamper byte 7 of m1 (shadow slot 1 = raw bytes [4, 8)); read offset 4.
    tamper_raw_byte(&m1, &mut store, 7, |_| 0x42);
    expect_host_read_mismatch(&m1, &mut store, 4, "m1 raw bit flip");
    Ok(())
}

// AN load validation. Naturally-aligned full-width i32 loads use the shadow as
// their sole source of truth, check `slot % A == 0`, and return that codeword
// directly. Unaligned and subword loads retain the raw/shadow equality check
// over every slot they touch.
//
// Genuine corruption is injected with `tamper_raw_byte` (an UNTRACKED
// `data_ptr` write): `Memory::data_mut` is unsuitable because it marks the
// memory whole-dirty and the shadow is re-encoded from raw before the guest
// runs (see `data_mut_between_calls_resynced_before_guest_load`), so a
// `data_mut` write is treated as legitimate and never traps.

const LOAD_CHECK_WAT: &str = r#"
    (module
        (import "env" "noop" (func $noop))
        (memory (export "m") 1)
        (func (export "store_i32") (param $a i32) (param $v i32)
            local.get $a local.get $v i32.store)
        (func (export "load_i32") (param $a i32) (result i32)
            local.get $a i32.load)
        (func (export "load_i32_8u") (param $a i32) (result i32)
            local.get $a i32.load8_u)
        (func (export "load_i32_16u") (param $a i32) (result i32)
            local.get $a i32.load16_u)
        (func (export "noop_host") (result i32)
            call $noop
            i32.const 0))
"#;

fn load_check_setup(a: u64) -> wasmtime::Result<(Store<()>, wasmtime::Instance, wasmtime::Memory)> {
    let mut config = make_config(true);
    config.an_constant(a);
    let engine = Engine::new(&config)?;
    let module = Module::new(&engine, LOAD_CHECK_WAT)?;
    let mut linker = Linker::new(&engine);
    linker.func_wrap("env", "noop", |_caller: wasmtime::Caller<'_, ()>| {})?;
    let mut store = Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &module)?;
    let mem = instance
        .get_memory(&mut store, "m")
        .expect("memory export missing");
    Ok((store, instance, mem))
}

#[test]
fn data_mut_between_calls_resynced_before_guest_load() -> wasmtime::Result<()> {
    // A *legitimate* host write via `Memory::data_mut` performed BETWEEN
    // top-level calls (outside any host call) marks the memory whole-dirty.
    // The aligned load reads the shadow as its source of truth, so the
    // whole-dirty memory MUST be re-encoded from raw before any guest code
    // runs — otherwise the guest would observe the stale shadow value.
    //
    // The heal happens at the host->wasm entry (`an_heal_whole_dirty` in
    // `invoke_wasm_and_catch_traps`). With it the guest observes the written
    // value rather than the stale shadow value.
    let (mut store, instance, mem) = load_check_setup(65521)?;
    let load = instance.get_typed_func::<i32, i32>(&mut store, "load_i32")?;
    // Non-zero address so a missing address decode can't pass on `A*0 == 0`.
    mem.data_mut(&mut store)[8..12].copy_from_slice(&0x42u32.to_le_bytes());
    let v = load
        .call(&mut store, 8)
        .expect("legitimate data_mut write must not false-trap the guest load");
    assert_eq!(v as u32, 0x42);
    Ok(())
}

#[test]
fn load_validity_check_clean_run_passes() -> wasmtime::Result<()> {
    // With the validity check on, a clean module with no tampering must still
    // load correctly. This guards against the check accidentally tripping
    // on the post-instantiation shadow state. Use UNALIGNED_WAT (exports
    // both store_i32 and load_i32) so we can write through the shadow-aware
    // store path and then read back through the validity-checked load path.
    let mut config = make_config(true);
    config.an_constant(65521);
    let engine = Engine::new(&config)?;
    let module = Module::new(&engine, UNALIGNED_WAT)?;
    let mut linker = Linker::new(&engine);
    linker.func_wrap("env", "noop", |_caller: wasmtime::Caller<'_, ()>| {})?;
    let mut store = Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &module)?;
    let store_fn = instance.get_typed_func::<(i32, i32), ()>(&mut store, "store_i32")?;
    let load_fn = instance.get_typed_func::<i32, i32>(&mut store, "load_i32")?;
    store_fn.call(&mut store, (8, 0x12345678))?;
    assert_eq!(load_fn.call(&mut store, 8)? as u32, 0x12345678);
    // Load at byte offset 12 (a zero slot) — must not trap.
    let _ = load_fn.call(&mut store, 12)?;
    Ok(())
}

#[test]
fn aligned_i32_load_uses_shadow_as_source_of_truth() -> wasmtime::Result<()> {
    // An aligned full-width load intentionally ignores a raw-only corruption
    // and returns the still-valid shadow codeword. A later consumer of the raw
    // memory remains responsible for detecting the divergence.
    let (mut store, instance, mem) = load_check_setup(65521)?;
    let load = instance.get_typed_func::<i32, i32>(&mut store, "load_i32")?;
    tamper_raw_byte(&mem, &mut store, 11, |_| 0x80);
    if wasmtime_environ::AN_ALIGNED_I32_LOAD_FROM_SHADOW {
        assert_eq!(load.call(&mut store, 8)?, 0);
    } else {
        expect_an_mismatch_trap(load.call(&mut store, 8), "raw tamper + baseline i32.load");
    }
    expect_host_read_mismatch(&mem, &mut store, 8, "raw tamper after aligned i32.load");
    Ok(())
}

#[test]
fn aligned_i32_load_traps_on_invalid_shadow_codeword() -> wasmtime::Result<()> {
    let (mut store, instance, mem) = load_check_setup(65521)?;
    let load = instance.get_typed_func::<i32, i32>(&mut store, "load_i32")?;
    let shadow = mem
        .an_shadow_data_mut_for_test(&mut store)
        .expect("shadow allocated under AN");
    // Raw address 8 maps to shadow byte offset 16. The all-zero slot is a
    // valid codeword; flipping its low bit makes the residue non-zero.
    shadow[16] ^= 1;
    if wasmtime_environ::AN_ALIGNED_I32_LOAD_FROM_SHADOW {
        expect_an_codeword_invalid_trap(load.call(&mut store, 8), "aligned shadow residue");
    } else {
        expect_an_mismatch_trap(load.call(&mut store, 8), "baseline shadow mismatch");
    }
    Ok(())
}

#[test]
fn aligned_i32_load_checks_exact_bounds_before_shadow() -> wasmtime::Result<()> {
    let (mut store, instance, _mem) = load_check_setup(65521)?;
    let load = instance.get_typed_func::<i32, i32>(&mut store, "load_i32")?;
    assert_eq!(load.call(&mut store, 65_532)?, 0);
    let err = load
        .call(&mut store, 65_536)
        .expect_err("aligned load just past one page must trap");
    let trap = err
        .downcast_ref::<wasmtime::Trap>()
        .unwrap_or_else(|| panic!("aligned out-of-bounds load was not a Trap: {err:?}"));
    assert_eq!(*trap, wasmtime::Trap::MemoryOutOfBounds);
    Ok(())
}

#[test]
fn load_validity_check_traps_on_load8u() -> wasmtime::Result<()> {
    // load8_u touches a single shadow slot. Tampering a byte inside that
    // slot must surface at the load.
    let (mut store, instance, mem) = load_check_setup(65521)?;
    let load8 = instance.get_typed_func::<i32, i32>(&mut store, "load_i32_8u")?;
    // Tamper a different byte (10) in the same slot as the non-zero loaded
    // address (8); the slot-level check still surfaces it.
    tamper_raw_byte(&mem, &mut store, 10, |b| b ^ 0x55);
    let res = load8.call(&mut store, 8);
    expect_an_mismatch_trap(res, "raw tamper + i32.load8_u");
    Ok(())
}

#[test]
fn load_validity_check_traps_on_load16u_cross_slot() -> wasmtime::Result<()> {
    // load16_u at byte offset 3 spans two shadow slots. Tampering at byte 4
    // (the second slot) must still surface at the load — confirming the
    // check fires on both touched slots.
    let (mut store, instance, mem) = load_check_setup(65521)?;
    let load16 = instance.get_typed_func::<i32, i32>(&mut store, "load_i32_16u")?;
    tamper_raw_byte(&mem, &mut store, 4, |_| 0xCD);
    let res = load16.call(&mut store, 3);
    expect_an_mismatch_trap(res, "raw tamper + cross-slot i32.load16_u");
    Ok(())
}

#[test]
fn load_validity_check_traps_unaligned_i32_load() -> wasmtime::Result<()> {
    // Unaligned i32.load (byte_pos != 0) spans two shadow slots. Tampering
    // either slot must surface.
    let (mut store, instance, mem) = load_check_setup(65521)?;
    let load = instance.get_typed_func::<i32, i32>(&mut store, "load_i32")?;
    // Load at byte offset 1: touches slot 0 (bytes 1..4) and slot 1 (byte 4).
    // Flip a bit in slot 1's raw byte 5 — only the second slot diverges.
    tamper_raw_byte(&mem, &mut store, 5, |b| b ^ 0x80);
    let res = load.call(&mut store, 1);
    expect_an_mismatch_trap(res, "unaligned i32.load second-slot tamper");
    Ok(())
}

#[test]
fn aligned_shadow_load_various_an_constants() -> wasmtime::Result<()> {
    for &a in &[1u64, 2, 6, 7, 1000, 1009, 65521, 16_777_215] {
        let (mut store, instance, _mem) = load_check_setup(a)?;
        let store_i32 = instance.get_typed_func::<(i32, i32), ()>(&mut store, "store_i32")?;
        let load = instance.get_typed_func::<i32, i32>(&mut store, "load_i32")?;
        store_i32.call(&mut store, (8, 0x1234_5678))?;
        assert_eq!(load.call(&mut store, 8)?, 0x1234_5678, "A={a}");
    }
    Ok(())
}

#[test]
fn aligned_shadow_load_rejects_invalid_even_an_constants() -> wasmtime::Result<()> {
    for &a in &[2u64, 6, 1000] {
        let (mut store, instance, mem) = load_check_setup(a)?;
        let load = instance.get_typed_func::<i32, i32>(&mut store, "load_i32")?;
        mem.an_shadow_data_mut_for_test(&mut store)
            .expect("shadow allocated under AN")[16] ^= 1;
        expect_an_codeword_invalid_trap(load.call(&mut store, 8), &format!("A={a}"));
    }
    Ok(())
}

// AN-encoding paths through component-model trampolines.
//
// The core `compile_wasm_to_array_trampoline` already emits the AN
// cross-check / resync libcalls around wasm→host calls, but components
// route imports through `compile_component_trampoline` (via
// `TrampolineCompiler::translate_hostcall`) which is a separate code path.
// Without the same hook there, a wasm-in-component store followed by a
// host import could leave the encoded shadow stale undetected.
//
// These tests cover the smoke path: a component with a core module that
// performs an `i32.store` and then invokes a host import. The clean run
// must complete without `AnMemoryMismatch` — i.e. the AN-encoding paths
// don't false-positive in the component-trampoline path AND the host
// boundary cross-check is wired correctly to the core caller's vmctx.
//
// Tampering core memory from the embedder to trigger a trap would require
// reaching into a component's nested core memory, which the public
// component API doesn't expose today. That side of coverage is deferred to
// future work (see AN_ENCODING_CHANGELOG.md).
mod component_an {
    use wasmtime::component::{Component, Linker};
    use wasmtime::{Config, Engine, Store, StoreContextMut};

    const COMPONENT_WAT: &str = r#"
        (component
            (import "noop" (func $noop))

            (core module $m
                (import "env" "noop" (func $noop))
                (memory (export "m") 1)
                (func (export "call") (result i32)
                    ;; Write a sentinel to the shadow-mirrored memory ...
                    i32.const 0
                    i32.const 0x1122_3344
                    i32.store
                    ;; ... and then cross the host boundary.
                    call $noop
                    ;; Return the sentinel, decoded via i32.load.
                    i32.const 0
                    i32.load))

            (core func $noop_lower (canon lower (func $noop)))
            (core instance $i (instantiate $m
                (with "env" (instance (export "noop" (func $noop_lower))))))

            (func (export "call") (result u32)
                (canon lift (core func $i "call"))))
    "#;

    fn make_engine(an_on: bool) -> wasmtime::Result<Engine> {
        let mut config = Config::new();
        // Component-model is on by default in the workspace test config,
        // but be explicit so the test is self-contained.
        config.wasm_component_model(true);
        config.an_encoding(an_on);
        if an_on {
            config.an_constant(65521);
        }
        Engine::new(&config)
    }

    fn instantiate(
        an_on: bool,
    ) -> wasmtime::Result<(Store<()>, wasmtime::component::TypedFunc<(), (u32,)>)> {
        let engine = make_engine(an_on)?;
        let component = Component::new(&engine, COMPONENT_WAT)?;
        let mut linker = Linker::new(&engine);
        linker.root().func_wrap("noop", |_store, _: ()| Ok(()))?;
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &component)?;
        let call = instance.get_typed_func::<(), (u32,)>(&mut store, "call")?;
        Ok((store, call))
    }

    #[test]
    fn component_compiles_without_an() -> wasmtime::Result<()> {
        // Baseline: same component must compile + run with AN off. Confirms
        // the wat itself is well-formed and that any failure in the AN-on
        // counterpart is specific to the AN path.
        let (mut store, call) = instantiate(false)?;
        let (v,) = call.call(&mut store, ())?;
        assert_eq!(v, 0x1122_3344);
        Ok(())
    }

    #[test]
    fn component_compiles_with_an() -> wasmtime::Result<()> {
        // AN on, no tampering: the hostcall trampoline must NOT false-
        // positive on the cross-check (the wasm store + the libcall path
        // both keep the shadow in lockstep with raw). If the cross-check
        // were misrouted to the component vmctx instead of the core caller
        // vmctx the call would crash; if it ran on the right vmctx but the
        // shadow update path were missing the call would trap with
        // `AnMemoryMismatch`. We assert neither happens.
        let (mut store, call) = instantiate(true)?;
        let (v,) = call.call(&mut store, ())?;
        assert_eq!(v, 0x1122_3344);
        Ok(())
    }

    #[test]
    fn component_with_an_various_constants() -> wasmtime::Result<()> {
        // Re-run the smoke test across A values to confirm the
        // hostcall-trampoline libcall reads `A` from the engine's tunables
        // (the resync re-encodes raw → shadow using that A; if it baked
        // the default A the next host call would surface a mismatch).
        for &a in &[1u64, 7, 1009, 65521, 16_777_215] {
            let mut config = Config::new();
            config.wasm_component_model(true);
            config.an_encoding(true);
            config.an_constant(a);
            let engine = Engine::new(&config)?;
            let component = Component::new(&engine, COMPONENT_WAT)?;
            let mut linker = Linker::new(&engine);
            linker.root().func_wrap("noop", |_store, _: ()| Ok(()))?;
            let mut store = Store::new(&engine, ());
            let instance = linker.instantiate(&mut store, &component)?;
            let call = instance.get_typed_func::<(), (u32,)>(&mut store, "call")?;
            let (v,) = call.call(&mut store, ())?;
            assert_eq!(v, 0x1122_3344, "A={a}");
        }
        Ok(())
    }

    // A component that transcodes a string between encodings (utf8 -> utf16).
    // Crossing encodings makes wasmtime synthesize a string-transcoder
    // trampoline. Under AN its ptr/len wasm args arrive encoded (`A*v`, widened
    // to I64) and its results must be re-encoded; before the fix the trampoline
    // applied `uextend.i64` to an already-i64 arg, which the aarch64 backend
    // rejected with `assert!(inner_bits < out_bits)` — i.e. compiling the
    // component panicked. This guards that compilation succeeds under AN.
    //
    // NOTE: this is intentionally compile-only — it isolates the cranelift
    // lowering panic. End-to-end string lifting/lowering under AN (including the
    // `may_enter`/`may_leave` instance-flag globals that once trapped "cannot
    // leave component instance") is covered separately by
    // `transcode_string_roundtrip_*`.
    const TRANSCODE_COMPONENT_WAT: &str = r#"
        (component
          (component $c
            (core module $m
              (func (export "") (param i32 i32) unreachable)
              (func (export "realloc") (param i32 i32 i32 i32) (result i32)
                unreachable)
              (memory (export "memory") 1))
            (core instance $m (instantiate $m))
            (func (export "a") (param "a" string)
              (canon lift (core func $m "")
                (realloc (func $m "realloc")) (memory $m "memory")
                string-encoding=utf16)))
          (component $c2
            (import "a" (func $f (param "a" string)))
            (core module $libc (memory (export "memory") 1))
            (core instance $libc (instantiate $libc))
            (core func $f (canon lower (func $f)
              string-encoding=utf8 (memory $libc "memory")))
            (core module $m
              (import "" "" (func $f (param i32 i32)))
              (func (export "f") (call $f (i32.const 0) (i32.const 4))))
            (core instance $m (instantiate $m
              (with "" (instance (export "" (func $f))))))
            (func (export "f") (canon lift (core func $m "f"))))
          (instance $c (instantiate $c))
          (instance $c2 (instantiate $c2 (with "a" (func $c "a"))))
          (export "f" (func $c2 "f")))
    "#;

    #[test]
    fn transcode_component_compiles_without_an() -> wasmtime::Result<()> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        let engine = Engine::new(&config)?;
        Component::new(&engine, TRANSCODE_COMPONENT_WAT)?;
        Ok(())
    }

    #[test]
    fn transcode_component_compiles_with_an() -> wasmtime::Result<()> {
        for &a in &[1u64, 7, 65521, 16_777_215] {
            let mut config = Config::new();
            config.wasm_component_model(true);
            config.an_encoding(true);
            config.an_constant(a);
            let engine = Engine::new(&config)?;
            // Must not panic in cranelift lowering of the transcoder trampoline.
            Component::new(&engine, TRANSCODE_COMPONENT_WAT)
                .unwrap_or_else(|e| panic!("compile failed under AN (A={a}): {e:?}"));
        }
        Ok(())
    }

    // End-to-end: lower a string from the host into a component under AN. This
    // exercises the whole string-ABI path that the inner_bits / "cannot leave"
    // bugs lived on: the canonical-ABI transcoder trampoline (decode i32 ptr/len
    // args, re-encode i32 results), the realloc call into AN-compiled core wasm,
    // and the `may_enter`/`may_leave` instance-flag globals (raw host storage,
    // encode-on-get / decode-on-set under AN). The core `strlen` simply returns
    // the byte length the ABI computed, so a correct result proves the bytes
    // were transcoded into core memory and the length round-tripped.
    const STRLEN_WAT: &str = r#"
        (component
          (core module $m
            (memory (export "memory") 1)
            ;; trivial bump allocator: small test strings fit at offset 16
            (func (export "realloc") (param i32 i32 i32 i32) (result i32)
              i32.const 16)
            (func (export "strlen") (param i32 i32) (result i32)
              local.get 1))
          (core instance $i (instantiate $m))
          (func (export "strlen") (param "s" string) (result u32)
            (canon lift (core func $i "strlen") (memory $i "memory")
              (realloc (func $i "realloc")) string-encoding=utf8)))
    "#;

    fn strlen_check(an_on: bool) -> wasmtime::Result<()> {
        let engine = make_engine(an_on)?;
        let component = Component::new(&engine, STRLEN_WAT)?;
        let linker = Linker::new(&engine);
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &component)?;
        let strlen = instance.get_typed_func::<(&str,), (u32,)>(&mut store, "strlen")?;

        // ASCII: byte length == char count.
        let (n,) = strlen.call(&mut store, ("hello",))?;
        assert_eq!(n, 5, "an_on={an_on}");

        // Multi-byte UTF-8: "é" is two bytes, so byte length is 6.
        let (n,) = strlen.call(&mut store, ("héllo",))?;
        assert_eq!(n, 6, "an_on={an_on}");
        Ok(())
    }

    #[test]
    fn transcode_string_roundtrip_without_an() -> wasmtime::Result<()> {
        strlen_check(false)
    }

    #[test]
    fn transcode_string_roundtrip_with_an() -> wasmtime::Result<()> {
        strlen_check(true)
    }

    // Like `STRLEN_WAT`, but the core function crosses a host boundary
    // (`call $noop`) *after* the host has lowered the string argument into
    // linear memory. The host-side lowering writes the string bytes raw via
    // the canonical ABI (`LowerContext`), so the AN-encoding shadow must be
    // re-encoded for the written range at the write site: the `call $noop`
    // boundary cross-check runs before any boundary resync could fix it up,
    // and a stale shadow falsely traps with `AnMemoryMismatch`.
    const STRLEN_BOUNDARY_WAT: &str = r#"
        (component
          (import "noop" (func $noop))
          (core module $m
            (import "env" "noop" (func $noop))
            (memory (export "memory") 1)
            ;; trivial bump allocator: small test strings fit at offset 16
            (func (export "realloc") (param i32 i32 i32 i32) (result i32)
              i32.const 16)
            (func (export "strlen") (param i32 i32) (result i32)
              call $noop
              local.get 1))
          (core func $noop_lower (canon lower (func $noop)))
          (core instance $i (instantiate $m
            (with "env" (instance (export "noop" (func $noop_lower))))))
          (func (export "strlen") (param "s" string) (result u32)
            (canon lift (core func $i "strlen") (memory $i "memory")
              (realloc (func $i "realloc")) string-encoding=utf8)))
    "#;

    fn strlen_boundary_check(an_on: bool, a: Option<u64>) -> wasmtime::Result<()> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.an_encoding(an_on);
        if let Some(a) = a {
            config.an_constant(a);
        }
        let engine = Engine::new(&config)?;
        let component = Component::new(&engine, STRLEN_BOUNDARY_WAT)?;
        let mut linker = Linker::new(&engine);
        linker.root().func_wrap("noop", |_store, _: ()| Ok(()))?;
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &component)?;
        let strlen = instance.get_typed_func::<(&str,), (u32,)>(&mut store, "strlen")?;

        // Called twice: the second call also proves the shadow ended up
        // consistent after the first call completed.
        let (n,) = strlen.call(&mut store, ("hello",))?;
        assert_eq!(n, 5, "an_on={an_on} a={a:?}");
        let (n,) = strlen.call(&mut store, ("héllo",))?;
        assert_eq!(n, 6, "an_on={an_on} a={a:?}");
        Ok(())
    }

    #[test]
    fn string_lowering_then_host_boundary_without_an() -> wasmtime::Result<()> {
        strlen_boundary_check(false, None)
    }

    #[test]
    fn string_lowering_then_host_boundary_with_an() -> wasmtime::Result<()> {
        strlen_boundary_check(true, None)
    }

    #[test]
    fn string_lowering_then_host_boundary_various_an_constants() -> wasmtime::Result<()> {
        for &a in &[1u64, 7, 1009, 65521, 16_777_215] {
            strlen_boundary_check(true, Some(a))?;
        }
        Ok(())
    }

    // A host result lowered into component core memory as `(tuple u8 u16)`.
    // Canonical ABI layout writes the fields separately at offsets 0 and 2,
    // leaving padding byte 1 untouched. Both dirty ranges share one 4-byte AN
    // slot. Flushing either range independently sees the other field's new
    // bytes as corruption; flushing their union validates only the padding.
    const PADDED_RESULT_WAT: &str = r#"
        (component
          (type $pair (tuple u8 u16))
          (import "make-pair" (func $make-pair (result $pair)))

          (core module $mem
            (memory (export "memory") 1))
          (core instance $mem (instantiate $mem))
          (core func $make-pair-lower
            (canon lower (func $make-pair) (memory $mem "memory")))

          (core module $m
            (import "" "make-pair" (func $make-pair (param i32)))
            (import "" "memory" (memory 1))
            (func (export "run") (result i32)
              ;; Seed the slot with non-output bytes so both field writes
              ;; visibly diverge from the old encoded shadow.
              i32.const 64
              i32.const -1
              i32.store
              i32.const 64
              call $make-pair
              i32.const 64
              i32.load))
          (core instance $m (instantiate $m
            (with "" (instance
              (export "make-pair" (func $make-pair-lower))
              (export "memory" (memory $mem "memory"))))))

          (func (export "run") (result u32)
            (canon lift (core func $m "run"))))
    "#;

    #[test]
    fn component_lowering_disjoint_same_slot_writes_resync() -> wasmtime::Result<()> {
        let engine = make_engine(true)?;
        let component = Component::new(&engine, PADDED_RESULT_WAT)?;
        let mut linker = Linker::new(&engine);
        linker
            .root()
            .func_wrap("make-pair", |_store, (): ()| Ok(((0x11u8, 0x2233u16),)))?;
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &component)?;
        let run = instance.get_typed_func::<(), (u32,)>(&mut store, "run")?;

        // byte 1 remains the seeded 0xff padding; the lowered fields occupy
        // byte 0 and bytes 2..4 respectively.
        assert_eq!(run.call(&mut store, ())?, (0x2233_ff11,));
        Ok(())
    }

    #[test]
    fn component_lowering_disjoint_same_slot_writes_reject_corrupt_padding() -> wasmtime::Result<()>
    {
        let engine = make_engine(true)?;
        let component = Component::new(&engine, PADDED_RESULT_WAT)?;
        let mut linker = Linker::new(&engine);
        linker.root().func_wrap(
            "make-pair",
            |store: StoreContextMut<'_, Option<wasmtime::Memory>>, (): ()| {
                let memory = store.data().expect("component core memory installed");
                let base = memory.data_ptr(&store);
                // The guest seeded byte 65 and mirrored it into the shadow
                // immediately before this call. Change only that padding byte
                // through the raw pointer, outside any legitimate host-write
                // API, so the dirty union must not exempt it from validation.
                unsafe {
                    base.add(65).write(0xEE);
                }
                Ok(((0x11u8, 0x2233u16),))
            },
        )?;
        let mut store = Store::new(&engine, None::<wasmtime::Memory>);
        let instance = linker.instantiate(&mut store, &component)?;
        let memory = instance
            .an_core_memory_for_test(&mut store, 0)
            .expect("component core memory available under AN");
        *store.data_mut() = Some(memory);
        let run = instance.get_typed_func::<(), (u32,)>(&mut store, "run")?;

        let err = run
            .call(&mut store, ())
            .expect_err("corrupt retained padding byte must trap");
        let trap = err
            .downcast_ref::<wasmtime::Trap>()
            .unwrap_or_else(|| panic!("component padding mismatch was not a trap: {err:?}"));
        assert_eq!(*trap, wasmtime::Trap::AnMemoryMismatch);
        Ok(())
    }

    // End-to-end resource new/drop under AN. `resource.new` goes through the
    // generic hostcall trampoline (which decodes its i32 rep), but `resource.drop`
    // uses a hand-written trampoline (`translate_resource_drop`) that must decode
    // the i32 handle index itself — before the fix the handle arrived encoded
    // (`A*1`) and the libcall reported "unknown handle index 65521".
    const RESOURCE_WAT: &str = r#"
        (component
          (type $r (resource (rep i32)))
          (core func $new (canon resource.new $r))
          (core func $drop (canon resource.drop $r))
          (core module $m
            (import "" "new" (func $new (param i32) (result i32)))
            (import "" "drop" (func $drop (param i32)))
            (func (export "run") (param i32) (result i32)
              ;; create a resource with the given rep, then drop its handle,
              ;; returning the handle index (observably non-zero on success)
              (local $h i32)
              (local.set $h (call $new (local.get 0)))
              (call $drop (local.get $h))
              (local.get $h)))
          (core instance $i (instantiate $m
            (with "" (instance
              (export "new" (func $new))
              (export "drop" (func $drop))))))
          (func (export "run") (param "rep" u32) (result u32)
            (canon lift (core func $i "run"))))
    "#;

    fn resource_check(an_on: bool) -> wasmtime::Result<()> {
        let engine = make_engine(an_on)?;
        let component = Component::new(&engine, RESOURCE_WAT)?;
        let linker = Linker::new(&engine);
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &component)?;
        let run = instance.get_typed_func::<(u32,), (u32,)>(&mut store, "run")?;
        // A successful new+drop hands back the (non-zero) handle index. Before
        // the AN fix this trapped with "unknown handle index 65521".
        let (h,) = run.call(&mut store, (42,))?;
        assert_ne!(h, 0, "an_on={an_on}");
        Ok(())
    }

    #[test]
    fn resource_new_drop_without_an() -> wasmtime::Result<()> {
        resource_check(false)
    }

    #[test]
    fn resource_new_drop_with_an() -> wasmtime::Result<()> {
        resource_check(true)
    }

    // Component LIFTING (reading a value OUT of guest core memory into the
    // host) must cross-check the read range against the encoded shadow under
    // verify-at-use, exactly like `Memory::read`/`Memory::data`. Here a guest
    // `start` writes the string "hello" at offset 16 of the (separately
    // instantiated, then imported) core memory, mirrored into the shadow.
    // `go` calls the host import `sink(string)`, whose canonical-ABI lowering
    // LIFTS [16,5) out of core memory to hand the host a `String`. A raw byte
    // tampered via the untracked `data_ptr` path must be caught when those
    // bytes are lifted.
    const LIFT_TAMPER_COMPONENT_WAT: &str = r#"
        (component
            (import "sink" (func $sink (param "s" string)))
            (core module $libc (memory (export "memory") 1))
            (core instance $libc (instantiate $libc))
            (core func $sink_lowered
                (canon lower (func $sink) string-encoding=utf8 (memory $libc "memory")))
            (core module $m
                (import "" "sink" (func $sink (param i32 i32)))
                (import "" "memory" (memory 1))
                (func $init
                    (i32.store8 (i32.const 16) (i32.const 104))  ;; 'h'
                    (i32.store8 (i32.const 17) (i32.const 101))  ;; 'e'
                    (i32.store8 (i32.const 18) (i32.const 108))  ;; 'l'
                    (i32.store8 (i32.const 19) (i32.const 108))  ;; 'l'
                    (i32.store8 (i32.const 20) (i32.const 111)))  ;; 'o'
                (start $init)
                (func (export "go")
                    (call $sink (i32.const 16) (i32.const 5))))
            (core instance $m (instantiate $m
                (with "" (instance
                    (export "sink" (func $sink_lowered))
                    (export "memory" (memory $libc "memory"))))))
            (func (export "go") (canon lift (core func $m "go"))))
    "#;

    #[test]
    fn component_lift_clean_run_passes() -> wasmtime::Result<()> {
        // Baseline: with no tampering the host sink must receive the lifted
        // "hello" and the cross-check must not false-positive.
        let engine = make_engine(true)?;
        let component = Component::new(&engine, LIFT_TAMPER_COMPONENT_WAT)?;
        let mut linker = Linker::new(&engine);
        linker.root().func_wrap("sink", |_store, (s,): (String,)| {
            assert_eq!(s, "hello");
            Ok(())
        })?;
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &component)?;
        let go = instance.get_typed_func::<(), ()>(&mut store, "go")?;
        go.call(&mut store, ())?;
        Ok(())
    }

    #[test]
    fn component_lift_tamper_traps() -> wasmtime::Result<()> {
        let engine = make_engine(true)?;
        let component = Component::new(&engine, LIFT_TAMPER_COMPONENT_WAT)?;
        let mut linker = Linker::new(&engine);
        linker
            .root()
            .func_wrap("sink", |_store, (_s,): (String,)| Ok(()))?;
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &component)?;

        // `start` wrote "hello" at offset 16 and mirrored it into the shadow.
        // Flip a raw byte via the untracked `data_ptr` path so raw[18] diverges
        // from the shadow without marking the memory whole-dirty.
        let memory = instance
            .an_core_memory_for_test(&mut store, 0)
            .expect("core memory identity under AN");
        super::tamper_raw_byte(&memory, &mut store, 18, |b| b ^ 0x40);

        // `go` calls `sink(string)`, lifting [16,5) out of the tampered core
        // memory. The lift cross-check must trap.
        let go = instance.get_typed_func::<(), ()>(&mut store, "go")?;
        let res = go.call(&mut store, ()).map(|()| 0);
        super::expect_an_mismatch_trap(res, "component lift tampered string");
        Ok(())
    }

    // A component that returns a `list<u32>` of the 16 bytes 0x00..0x0f lifted
    // out of its core memory (the canonical return-area pattern: the core func
    // stores [ptr=8, len=4] at address 100 and returns 100).
    const LIST_RET_COMPONENT_WAT: &str = r#"
        (component
            (core module $m
                (memory (export "memory") 1)
                (func (export "list32") (result i32)
                    (i32.store offset=0 (i32.const 100) (i32.const 8))
                    (i32.store offset=4 (i32.const 100) (i32.const 4))
                    i32.const 100)
                (data (i32.const 8) "\00\01\02\03\04\05\06\07\08\09\0a\0b\0c\0d\0e\0f"))
            (core instance $i (instantiate $m))
            (func (export "list-u32") (result (list u32))
                (canon lift (core func $i "list32") (memory $i "memory"))))
    "#;

    fn lifted_list_u32(
        store: &mut Store<()>,
        instance: &wasmtime::component::Instance,
    ) -> wasmtime::Result<wasmtime::component::WasmList<u32>> {
        use wasmtime::component::WasmList;
        Ok(instance
            .get_typed_func::<(), (WasmList<u32>,)>(&mut *store, "list-u32")?
            .call(&mut *store, ())?
            .0)
    }

    #[test]
    fn try_as_le_slice_clean_and_tamper() -> wasmtime::Result<()> {
        let engine = make_engine(true)?;
        let component = Component::new(&engine, LIST_RET_COMPONENT_WAT)?;
        let mut store = Store::new(&engine, ());
        let instance = Linker::new(&engine).instantiate(&mut store, &component)?;
        let list = lifted_list_u32(&mut store, &instance)?;

        // Clean: the fallible twin returns the lifted slice verbatim.
        assert_eq!(
            list.try_as_le_slice(&store)?,
            [
                u32::to_le(0x03_02_01_00),
                u32::to_le(0x07_06_05_04),
                u32::to_le(0x0b_0a_09_08),
                u32::to_le(0x0f_0e_0d_0c),
            ]
        );

        // Tamper a raw byte inside the list's data range [8, 24) via the
        // untracked `data_ptr` path, diverging raw from the encoded shadow.
        let memory = instance
            .an_core_memory_for_test(&mut store, 0)
            .expect("core memory identity under AN");
        super::tamper_raw_byte(&memory, &mut store, 10, |b| b ^ 0x40);

        // The fallible twin surfaces the mismatch as a trap-carrying error
        // instead of panicking like `as_le_slice`.
        let err = list
            .try_as_le_slice(&store)
            .expect_err("try_as_le_slice: expected AnMemoryMismatch, got Ok");
        let trap = err
            .downcast_ref::<wasmtime::Trap>()
            .unwrap_or_else(|| panic!("try_as_le_slice: not a Trap: {err:?}"));
        assert_eq!(*trap, wasmtime::Trap::AnMemoryMismatch);
        Ok(())
    }

    #[test]
    #[should_panic(expected = "AnMemoryMismatch")]
    fn as_le_slice_panics_on_tamper() {
        let engine = make_engine(true).unwrap();
        let component = Component::new(&engine, LIST_RET_COMPONENT_WAT).unwrap();
        let mut store = Store::new(&engine, ());
        let instance = Linker::new(&engine)
            .instantiate(&mut store, &component)
            .unwrap();
        let list = lifted_list_u32(&mut store, &instance).unwrap();
        let memory = instance
            .an_core_memory_for_test(&mut store, 0)
            .expect("core memory identity under AN");
        super::tamper_raw_byte(&memory, &mut store, 10, |b| b ^ 0x40);
        let _ = list.as_le_slice(&store);
    }
}

// ---------------------------------------------------------------------------
// Boundary codeword validity check
//
// The wasm/host trampolines decode encoded i32 values (`A*v` widened to I64)
// into raw i32 in two places:
//
//   * `compile_wasm_to_array_trampoline`: encoded args from wasm → raw i32
//     in the ValRaw slots passed to the host.
//   * `array_to_wasm_trampoline`: encoded results from wasm → raw i32 in the
//     ValRaw slots returned to the host.
//
// Always-on when AN-encoding is on: before each `udiv` decode, emit
// `val % A`; if non-zero, raise `Trap::AnCodewordInvalid`. Defends against
// external corruption (bit flip in a register / on the wasm operand stack).
//
// The check can never fire in valid wasm: every codepath that places an
// encoded I64 on the operand stack multiplies by `A` at some point, so the
// invariant `val % A == 0` is structurally maintained. To exercise the
// trap-fires path the test-only `Config::an_inject_codeword_fault(offset)`
// knob causes the trampoline to add `offset` to the first encoded i32
// arg/result BEFORE the modulo check fires. Any offset in `(0, A)`
// guarantees a non-multiple.
mod codeword_check {
    use wasmtime::{Caller, Config, Engine, Linker, Module, Store};

    // Module with one host import (i32 -> i32) and a wasm wrapper that calls
    // it through the wasm→host trampoline.
    const ECHO_WAT: &str = r#"
        (module
            (import "host" "echo" (func $echo (param i32) (result i32)))
            (func (export "run") (param i32) (result i32)
                (call $echo (local.get 0))))
    "#;

    // Multi-i32-arg module.
    const SUM3_WAT: &str = r#"
        (module
            (import "host" "sum3" (func $sum3 (param i32 i32 i32) (result i32)))
            (func (export "run") (param i32 i32 i32) (result i32)
                (call $sum3 (local.get 0) (local.get 1) (local.get 2))))
    "#;

    // i64-only host import to confirm the wider scalar uses the same boundary
    // encode/decode path as i32.
    const I64_ECHO_WAT: &str = r#"
        (module
            (import "host" "mix" (func $mix (param i64 i64) (result i64)))
            (func (export "run") (param i64 i64) (result i64)
                (call $mix (local.get 0) (local.get 1))))
    "#;

    // Pure host → wasm return-path module: no host import, just an i32 result.
    const RETURN_WAT: &str = r#"
        (module
            (func (export "ret") (param i32) (result i32)
                (i32.add (local.get 0) (i32.const 100))))
    "#;

    const I64_RETURN_WAT: &str = r#"
        (module
            (func (export "ret") (param i64) (result i64)
                (i64.add (local.get 0) (i64.const 100))))
    "#;

    fn make_config(an_on: bool, a: Option<u64>, fault: Option<u64>) -> Config {
        let mut config = Config::new();
        config.an_encoding(an_on);
        if let Some(a) = a {
            config.an_constant(a);
        }
        if let Some(offset) = fault {
            config.an_inject_codeword_fault(offset);
        }
        config
    }

    // -------- positive (no false positive) --------

    #[test]
    fn codeword_check_clean_wasm_to_host_args() -> wasmtime::Result<()> {
        let engine = Engine::new(&make_config(true, None, None))?;
        let module = Module::new(&engine, ECHO_WAT)?;
        let mut linker: Linker<()> = Linker::new(&engine);
        linker.func_wrap("host", "echo", |_: Caller<'_, ()>, x: i32| x * 2)?;
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &module)?;
        let run = instance.get_typed_func::<i32, i32>(&mut store, "run")?;
        assert_eq!(run.call(&mut store, 21)?, 42);
        assert_eq!(run.call(&mut store, -7)?, -14);
        assert_eq!(run.call(&mut store, 0)?, 0);
        Ok(())
    }

    #[test]
    fn codeword_check_clean_wasm_to_host_multi_args() -> wasmtime::Result<()> {
        let engine = Engine::new(&make_config(true, None, None))?;
        let module = Module::new(&engine, SUM3_WAT)?;
        let mut linker: Linker<()> = Linker::new(&engine);
        linker.func_wrap(
            "host",
            "sum3",
            |_: Caller<'_, ()>, a: i32, b: i32, c: i32| a.wrapping_add(b).wrapping_add(c),
        )?;
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &module)?;
        let run = instance.get_typed_func::<(i32, i32, i32), i32>(&mut store, "run")?;
        assert_eq!(run.call(&mut store, (1, 2, 3))?, 6);
        assert_eq!(run.call(&mut store, (i32::MAX, 1, 0))?, i32::MIN);
        Ok(())
    }

    #[test]
    fn codeword_check_clean_wasm_to_host_i64_params() -> wasmtime::Result<()> {
        let engine = Engine::new(&make_config(true, None, None))?;
        let module = Module::new(&engine, I64_ECHO_WAT)?;
        let mut linker: Linker<()> = Linker::new(&engine);
        linker.func_wrap("host", "mix", |_: Caller<'_, ()>, a: i64, b: i64| -> i64 {
            a + b + 1
        })?;
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &module)?;
        let run = instance.get_typed_func::<(i64, i64), i64>(&mut store, "run")?;
        assert_eq!(run.call(&mut store, (1, 40))?, 42);
        assert_eq!(run.call(&mut store, (-1, 1))?, 1);
        Ok(())
    }

    #[test]
    fn codeword_check_clean_host_to_wasm_returns() -> wasmtime::Result<()> {
        // Exercises the `array_to_wasm_trampoline` decode path: Rust calls
        // wasm, wasm returns an i32, decode + check at the trampoline.
        let engine = Engine::new(&make_config(true, None, None))?;
        let module = Module::new(&engine, RETURN_WAT)?;
        let mut store = Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
        let f = instance.get_typed_func::<i32, i32>(&mut store, "ret")?;
        assert_eq!(f.call(&mut store, 5)?, 105);
        assert_eq!(f.call(&mut store, -10)?, 90);
        Ok(())
    }

    #[test]
    fn codeword_check_clean_host_to_wasm_i64_returns() -> wasmtime::Result<()> {
        let engine = Engine::new(&make_config(true, None, None))?;
        let module = Module::new(&engine, I64_RETURN_WAT)?;
        let mut store = Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
        let f = instance.get_typed_func::<i64, i64>(&mut store, "ret")?;
        assert_eq!(f.call(&mut store, 5)?, 105);
        assert_eq!(f.call(&mut store, -10)?, 90);
        assert_eq!(f.call(&mut store, i64::MAX)?, i64::MIN + 99);
        Ok(())
    }

    #[test]
    fn codeword_check_clean_repeated_host_calls() -> wasmtime::Result<()> {
        let engine = Engine::new(&make_config(true, None, None))?;
        let module = Module::new(&engine, ECHO_WAT)?;
        let mut linker: Linker<()> = Linker::new(&engine);
        linker.func_wrap("host", "echo", |_: Caller<'_, ()>, x: i32| x + 1)?;
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &module)?;
        let run = instance.get_typed_func::<i32, i32>(&mut store, "run")?;
        for i in 0..50 {
            assert_eq!(run.call(&mut store, i)?, i + 1);
        }
        Ok(())
    }

    #[test]
    fn codeword_check_clean_various_an_constants() -> wasmtime::Result<()> {
        for &a in &[1u64, 7, 1009, 65521, 16_777_215] {
            let engine = Engine::new(&make_config(true, Some(a), None))?;
            let module = Module::new(&engine, ECHO_WAT)?;
            let mut linker: Linker<()> = Linker::new(&engine);
            linker.func_wrap("host", "echo", |_: Caller<'_, ()>, x: i32| x * 3)?;
            let mut store = Store::new(&engine, ());
            let instance = linker.instantiate(&mut store, &module)?;
            let run = instance.get_typed_func::<i32, i32>(&mut store, "run")?;
            assert_eq!(run.call(&mut store, 14)?, 42, "A={a}");
            assert_eq!(run.call(&mut store, -1)?, -3, "A={a}");
        }
        Ok(())
    }

    // -------- negative (trap fires) --------

    fn assert_codeword_trap(err: wasmtime::Error) {
        let trap = err
            .downcast::<wasmtime::Trap>()
            .expect("expected a wasm Trap");
        assert_eq!(
            trap,
            wasmtime::Trap::AnCodewordInvalid,
            "expected AnCodewordInvalid, got {trap:?}"
        );
    }

    #[test]
    fn codeword_check_traps_wasm_to_host_args_with_injection() -> wasmtime::Result<()> {
        // Inject offset 1: every encoded arg gets `+1`, which is never a
        // multiple of A (for A > 1). The wasm→host trampoline modulo check
        // must trap.
        let engine = Engine::new(&make_config(true, None, Some(1)))?;
        let module = Module::new(&engine, ECHO_WAT)?;
        let mut linker: Linker<()> = Linker::new(&engine);
        linker.func_wrap("host", "echo", |_: Caller<'_, ()>, x: i32| x)?;
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &module)?;
        let run = instance.get_typed_func::<i32, i32>(&mut store, "run")?;
        let err = run.call(&mut store, 5).expect_err("expected trap");
        assert_codeword_trap(err);
        Ok(())
    }

    #[test]
    fn codeword_check_traps_wasm_to_host_i64_args_with_injection() -> wasmtime::Result<()> {
        let engine = Engine::new(&make_config(true, None, Some(1)))?;
        let module = Module::new(&engine, I64_ECHO_WAT)?;
        let mut linker: Linker<()> = Linker::new(&engine);
        linker.func_wrap("host", "mix", |_: Caller<'_, ()>, a: i64, b: i64| -> i64 {
            a + b
        })?;
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &module)?;
        let run = instance.get_typed_func::<(i64, i64), i64>(&mut store, "run")?;
        let err = run.call(&mut store, (5, 7)).expect_err("expected trap");
        assert_codeword_trap(err);
        Ok(())
    }

    #[test]
    fn codeword_check_traps_host_to_wasm_returns_with_injection() -> wasmtime::Result<()> {
        // The same fault-inject knob also corrupts the first i32 result on
        // the array_to_wasm path (the trampoline used when the host calls
        // back into wasm via `instance.get_typed_func`).
        let engine = Engine::new(&make_config(true, None, Some(1)))?;
        let module = Module::new(&engine, RETURN_WAT)?;
        let mut store = Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
        let f = instance.get_typed_func::<i32, i32>(&mut store, "ret")?;
        let err = f.call(&mut store, 5).expect_err("expected trap");
        assert_codeword_trap(err);
        Ok(())
    }

    #[test]
    fn codeword_check_traps_host_to_wasm_i64_returns_with_injection() -> wasmtime::Result<()> {
        let engine = Engine::new(&make_config(true, None, Some(1)))?;
        let module = Module::new(&engine, I64_RETURN_WAT)?;
        let mut store = Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
        let f = instance.get_typed_func::<i64, i64>(&mut store, "ret")?;
        let err = f.call(&mut store, 5).expect_err("expected trap");
        assert_codeword_trap(err);
        Ok(())
    }

    #[test]
    fn codeword_check_traps_various_an_constants() -> wasmtime::Result<()> {
        // For each A > 1, offset 1 is guaranteed in (0, A) so the check
        // must fire. (A = 1 is a degenerate case where every i64 is a
        // multiple of 1; the check is skipped at A=1 and corruption goes
        // undetected by design.)
        for &a in &[7u64, 1009, 65521, 16_777_215] {
            let engine = Engine::new(&make_config(true, Some(a), Some(1)))?;
            let module = Module::new(&engine, ECHO_WAT)?;
            let mut linker: Linker<()> = Linker::new(&engine);
            linker.func_wrap("host", "echo", |_: Caller<'_, ()>, x: i32| x)?;
            let mut store = Store::new(&engine, ());
            let instance = linker.instantiate(&mut store, &module)?;
            let run = instance.get_typed_func::<i32, i32>(&mut store, "run")?;
            let err = run.call(&mut store, 3).expect_err("expected trap");
            assert_codeword_trap(err);
        }
        Ok(())
    }

    #[test]
    fn codeword_check_no_trap_when_an_off() -> wasmtime::Result<()> {
        // With AN off the trampoline never emits the check, so the run is
        // clean regardless of the fault-inject knob.
        let engine = Engine::new(&make_config(false, None, Some(1)))?;
        let module = Module::new(&engine, ECHO_WAT)?;
        let mut linker: Linker<()> = Linker::new(&engine);
        linker.func_wrap("host", "echo", |_: Caller<'_, ()>, x: i32| x + 1)?;
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &module)?;
        let run = instance.get_typed_func::<i32, i32>(&mut store, "run")?;
        assert_eq!(run.call(&mut store, 10)?, 11);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Component-model boundary: i32 args + i32 results + codeword check
//
// The component hookup added the memory cross-check + resync libcalls around
// the component hostcall trampoline (`translate_hostcall` in
// `crates/cranelift/src/compiler/component.rs`) but did NOT decode
// AN-encoded i32 wasm params before they reach the host, nor encode i32
// results coming back. As a result, components with `i32`-typed import
// args/results were silently broken under AN (host received encoded I64
// instead of raw i32, and wasm received raw i32 where it expected
// encoded I64).
//
// These tests exercise components with `u32`-typed component imports
// (which lower to core wasm `i32`) and assert:
//
//   * clean run produces the expected value across all legal A's
//   * the boundary codeword check fires on the component path too
//     (fault-injected to force a non-codeword crossing the boundary).
mod component_codeword {
    use wasmtime::component::{Component, Linker};
    use wasmtime::{Config, Engine, Store};

    // Component with a u32 → u32 host import called from inside core wasm.
    const ECHO_COMPONENT_WAT: &str = r#"
        (component
            (import "echo" (func $echo (param "x" u32) (result u32)))

            (core module $m
                (import "env" "echo" (func $echo (param i32) (result i32)))
                (func (export "call") (param i32) (result i32)
                    (call $echo (local.get 0))))

            (core func $echo_lower (canon lower (func $echo)))
            (core instance $i (instantiate $m
                (with "env" (instance (export "echo" (func $echo_lower))))))

            (func (export "call") (param "x" u32) (result u32)
                (canon lift (core func $i "call"))))
    "#;

    // Component with multi-i32-arg host import.
    const SUM3_COMPONENT_WAT: &str = r#"
        (component
            (import "sum3" (func $sum3 (param "a" u32) (param "b" u32) (param "c" u32) (result u32)))

            (core module $m
                (import "env" "sum3" (func $sum3 (param i32 i32 i32) (result i32)))
                (func (export "call") (param i32 i32 i32) (result i32)
                    (call $sum3 (local.get 0) (local.get 1) (local.get 2))))

            (core func $sum3_lower (canon lower (func $sum3)))
            (core instance $i (instantiate $m
                (with "env" (instance (export "sum3" (func $sum3_lower))))))

            (func (export "call") (param "a" u32) (param "b" u32) (param "c" u32) (result u32)
                (canon lift (core func $i "call"))))
    "#;

    // Same shape as `ECHO_COMPONENT_WAT`, but with an i64/s64 scalar. This
    // exercises the wider AN boundary: encoded core `i64` values are I128 in
    // wasm and raw i64 in the component host import.
    const I64_ECHO_COMPONENT_WAT: &str = r#"
        (component
            (import "echo64" (func $echo64 (param "x" s64) (result s64)))

            (core module $m
                (import "env" "echo64" (func $echo64 (param i64) (result i64)))
                (func (export "call") (param i64) (result i64)
                    (call $echo64 (local.get 0))))

            (core func $echo64_lower (canon lower (func $echo64)))
            (core instance $i (instantiate $m
                (with "env" (instance (export "echo64" (func $echo64_lower))))))

            (func (export "call") (param "x" s64) (result s64)
                (canon lift (core func $i "call"))))
    "#;

    fn make_config(an_on: bool, a: Option<u64>, fault: Option<u64>) -> Config {
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.an_encoding(an_on);
        if let Some(a) = a {
            config.an_constant(a);
        }
        if let Some(offset) = fault {
            config.an_inject_codeword_fault(offset);
        }
        config
    }

    #[test]
    fn component_i32_arg_passthrough_without_an() -> wasmtime::Result<()> {
        // Baseline: confirm the wat itself works with AN off, so a failure
        // in the AN-on counterpart is specific to the component decode path.
        let engine = Engine::new(&make_config(false, None, None))?;
        let component = Component::new(&engine, ECHO_COMPONENT_WAT)?;
        let mut linker = Linker::new(&engine);
        linker
            .root()
            .func_wrap("echo", |_store, (x,): (u32,)| Ok((x.wrapping_mul(2),)))?;
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &component)?;
        let call = instance.get_typed_func::<(u32,), (u32,)>(&mut store, "call")?;
        assert_eq!(call.call(&mut store, (21,))?, (42,));
        Ok(())
    }

    #[test]
    fn component_i32_arg_passthrough_with_an() -> wasmtime::Result<()> {
        // Real fix: component imports with i32 args round-trip through the
        // hostcall trampoline. Before the fix this would either pass an
        // encoded I64 to host (host saw A*x instead of x → wrong return), or
        // crash on a type mismatch.
        let engine = Engine::new(&make_config(true, None, None))?;
        let component = Component::new(&engine, ECHO_COMPONENT_WAT)?;
        let mut linker = Linker::new(&engine);
        linker
            .root()
            .func_wrap("echo", |_store, (x,): (u32,)| Ok((x.wrapping_mul(2),)))?;
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &component)?;
        let call = instance.get_typed_func::<(u32,), (u32,)>(&mut store, "call")?;
        assert_eq!(call.call(&mut store, (21,))?, (42,));
        assert_eq!(call.call(&mut store, (0,))?, (0,));
        assert_eq!(call.call(&mut store, (123_456,))?, (246_912,));
        Ok(())
    }

    #[test]
    fn component_i32_multi_arg_with_an() -> wasmtime::Result<()> {
        let engine = Engine::new(&make_config(true, None, None))?;
        let component = Component::new(&engine, SUM3_COMPONENT_WAT)?;
        let mut linker = Linker::new(&engine);
        linker
            .root()
            .func_wrap("sum3", |_store, (a, b, c): (u32, u32, u32)| {
                Ok((a.wrapping_add(b).wrapping_add(c),))
            })?;
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &component)?;
        let call = instance.get_typed_func::<(u32, u32, u32), (u32,)>(&mut store, "call")?;
        assert_eq!(call.call(&mut store, (1, 2, 3))?, (6,));
        assert_eq!(call.call(&mut store, (10, 20, 30))?, (60,));
        Ok(())
    }

    #[test]
    fn component_i64_arg_passthrough_without_an() -> wasmtime::Result<()> {
        let engine = Engine::new(&make_config(false, None, None))?;
        let component = Component::new(&engine, I64_ECHO_COMPONENT_WAT)?;
        let mut linker = Linker::new(&engine);
        linker
            .root()
            .func_wrap("echo64", |_store, (x,): (i64,)| Ok((x.wrapping_add(7),)))?;
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &component)?;
        let call = instance.get_typed_func::<(i64,), (i64,)>(&mut store, "call")?;
        assert_eq!(call.call(&mut store, (35,))?, (42,));
        Ok(())
    }

    #[test]
    fn component_i64_arg_passthrough_with_an() -> wasmtime::Result<()> {
        let engine = Engine::new(&make_config(true, None, None))?;
        let component = Component::new(&engine, I64_ECHO_COMPONENT_WAT)?;
        let mut linker = Linker::new(&engine);
        linker
            .root()
            .func_wrap("echo64", |_store, (x,): (i64,)| Ok((x.wrapping_mul(2),)))?;
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &component)?;
        let call = instance.get_typed_func::<(i64,), (i64,)>(&mut store, "call")?;
        for &x in &[0, 1, -1, 21, -21, i64::MIN, i64::MAX] {
            assert_eq!(call.call(&mut store, (x,))?, (x.wrapping_mul(2),));
        }
        Ok(())
    }

    #[test]
    fn component_i32_various_an_constants() -> wasmtime::Result<()> {
        for &a in &[1u64, 7, 1009, 65521, 16_777_215] {
            let engine = Engine::new(&make_config(true, Some(a), None))?;
            let component = Component::new(&engine, ECHO_COMPONENT_WAT)?;
            let mut linker = Linker::new(&engine);
            linker
                .root()
                .func_wrap("echo", |_store, (x,): (u32,)| Ok((x.wrapping_add(7),)))?;
            let mut store = Store::new(&engine, ());
            let instance = linker.instantiate(&mut store, &component)?;
            let call = instance.get_typed_func::<(u32,), (u32,)>(&mut store, "call")?;
            assert_eq!(call.call(&mut store, (35,))?, (42,), "A={a}");
        }
        Ok(())
    }

    #[test]
    fn component_i64_various_an_constants() -> wasmtime::Result<()> {
        for &a in &[1u64, 7, 1009, 65521, 16_777_215] {
            let engine = Engine::new(&make_config(true, Some(a), None))?;
            let component = Component::new(&engine, I64_ECHO_COMPONENT_WAT)?;
            let mut linker = Linker::new(&engine);
            linker
                .root()
                .func_wrap("echo64", |_store, (x,): (i64,)| Ok((x.wrapping_add(7),)))?;
            let mut store = Store::new(&engine, ());
            let instance = linker.instantiate(&mut store, &component)?;
            let call = instance.get_typed_func::<(i64,), (i64,)>(&mut store, "call")?;
            assert_eq!(call.call(&mut store, (35,))?, (42,), "A={a}");
            assert_eq!(call.call(&mut store, (-49,))?, (-42,), "A={a}");
        }
        Ok(())
    }

    #[test]
    fn component_codeword_check_traps_with_injection() -> wasmtime::Result<()> {
        // Fault-inject offset 1 on the component path: the first encoded i32
        // arg gets bumped by 1 before the modulo check fires. With A > 1
        // the codeword check must trap.
        let engine = Engine::new(&make_config(true, None, Some(1)))?;
        let component = Component::new(&engine, ECHO_COMPONENT_WAT)?;
        let mut linker = Linker::new(&engine);
        linker
            .root()
            .func_wrap("echo", |_store, (x,): (u32,)| Ok((x,)))?;
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &component)?;
        let call = instance.get_typed_func::<(u32,), (u32,)>(&mut store, "call")?;
        let err = call.call(&mut store, (5,)).expect_err("expected trap");
        let trap = err
            .downcast::<wasmtime::Trap>()
            .expect("expected a wasm Trap");
        assert_eq!(trap, wasmtime::Trap::AnCodewordInvalid);
        Ok(())
    }

    #[test]
    fn component_i64_codeword_check_traps_with_injection() -> wasmtime::Result<()> {
        let engine = Engine::new(&make_config(true, None, Some(1)))?;
        let component = Component::new(&engine, I64_ECHO_COMPONENT_WAT)?;
        let mut linker = Linker::new(&engine);
        linker
            .root()
            .func_wrap("echo64", |_store, (x,): (i64,)| Ok((x,)))?;
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &component)?;
        let call = instance.get_typed_func::<(i64,), (i64,)>(&mut store, "call")?;
        let err = call.call(&mut store, (5,)).expect_err("expected trap");
        let trap = err
            .downcast::<wasmtime::Trap>()
            .expect("expected a wasm Trap");
        assert_eq!(trap, wasmtime::Trap::AnCodewordInvalid);
        Ok(())
    }
}

// Cross-type conversion operators under AN. The float-containing module is kept
// as an AN-off oracle and an AN-on refusal test; the integer-only companion below
// covers the supported i32<->i64 conversions.
mod conversions {
    use wasmtime::{Config, Engine, Module, Store};

    const CONV_WAT: &str = include_str!("../../an_encoding/conversions.wat");

    struct ConvInstance {
        store: Store<()>,
        instance: wasmtime::Instance,
    }

    fn make_config(an_on: bool, a: Option<u64>) -> Config {
        let mut config = Config::new();
        config.an_encoding(an_on);
        if let Some(a) = a {
            config.an_constant(a);
        }
        config
    }

    fn make_inst(an_on: bool, a: Option<u64>) -> wasmtime::Result<ConvInstance> {
        let engine = Engine::new(&make_config(an_on, a))?;
        let module = Module::new(&engine, CONV_WAT)?;
        let mut store = Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
        Ok(ConvInstance { store, instance })
    }

    fn call_i32_i32(c: &mut ConvInstance, name: &str, x: i32) -> wasmtime::Result<i32> {
        let f = c.instance.get_typed_func::<i32, i32>(&mut c.store, name)?;
        f.call(&mut c.store, x)
    }
    fn call_i32_i64(c: &mut ConvInstance, name: &str, x: i32) -> wasmtime::Result<i64> {
        let f = c.instance.get_typed_func::<i32, i64>(&mut c.store, name)?;
        f.call(&mut c.store, x)
    }
    fn call_i64_i32(c: &mut ConvInstance, name: &str, x: i64) -> wasmtime::Result<i32> {
        let f = c.instance.get_typed_func::<i64, i32>(&mut c.store, name)?;
        f.call(&mut c.store, x)
    }
    fn call_f32_i32(c: &mut ConvInstance, name: &str, x: f32) -> wasmtime::Result<i32> {
        let f = c.instance.get_typed_func::<f32, i32>(&mut c.store, name)?;
        f.call(&mut c.store, x)
    }
    fn call_f64_i32(c: &mut ConvInstance, name: &str, x: f64) -> wasmtime::Result<i32> {
        let f = c.instance.get_typed_func::<f64, i32>(&mut c.store, name)?;
        f.call(&mut c.store, x)
    }
    fn call_i32_f32(c: &mut ConvInstance, name: &str, x: i32) -> wasmtime::Result<f32> {
        let f = c.instance.get_typed_func::<i32, f32>(&mut c.store, name)?;
        f.call(&mut c.store, x)
    }
    fn call_i32_f64(c: &mut ConvInstance, name: &str, x: i32) -> wasmtime::Result<f64> {
        let f = c.instance.get_typed_func::<i32, f64>(&mut c.store, name)?;
        f.call(&mut c.store, x)
    }
    #[allow(dead_code)]
    fn call_f32_f32(c: &mut ConvInstance, name: &str, x: f32) -> wasmtime::Result<f32> {
        let f = c.instance.get_typed_func::<f32, f32>(&mut c.store, name)?;
        f.call(&mut c.store, x)
    }

    fn assert_trap<T: std::fmt::Debug>(
        res: wasmtime::Result<T>,
        expected: wasmtime::Trap,
        label: &str,
    ) {
        let err = res.expect_err(&format!("{label}: expected trap, got Ok"));
        let trap = err
            .downcast_ref::<wasmtime::Trap>()
            .unwrap_or_else(|| panic!("{label}: not a Trap: {err:?}"));
        assert_eq!(*trap, expected, "{label}: trap code mismatch");
    }

    /// Run every conversion-correctness assertion. Same expected results
    /// under AN-on and AN-off.
    fn correctness_assertions(c: &mut ConvInstance) -> wasmtime::Result<()> {
        // i32.extend8_s
        assert_eq!(call_i32_i32(c, "ext8_s", 0)?, 0);
        assert_eq!(call_i32_i32(c, "ext8_s", 127)?, 127);
        assert_eq!(call_i32_i32(c, "ext8_s", 128)?, -128);
        assert_eq!(call_i32_i32(c, "ext8_s", 255)?, -1);
        assert_eq!(call_i32_i32(c, "ext8_s", 0x100)?, 0);
        assert_eq!(call_i32_i32(c, "ext8_s", 0x1ff)?, -1);
        assert_eq!(call_i32_i32(c, "ext8_s", -1)?, -1);

        // i32.extend16_s
        assert_eq!(call_i32_i32(c, "ext16_s", 0)?, 0);
        assert_eq!(call_i32_i32(c, "ext16_s", 32767)?, 32767);
        assert_eq!(call_i32_i32(c, "ext16_s", 32768)?, -32768);
        assert_eq!(call_i32_i32(c, "ext16_s", 0xffff)?, -1);
        assert_eq!(call_i32_i32(c, "ext16_s", 0x10000)?, 0);
        assert_eq!(call_i32_i32(c, "ext16_s", -1)?, -1);

        // i32.wrap_i64
        assert_eq!(call_i64_i32(c, "wrap", 0)?, 0);
        assert_eq!(call_i64_i32(c, "wrap", i32::MAX as i64)?, i32::MAX);
        assert_eq!(call_i64_i32(c, "wrap", -1_i64)?, -1);
        assert_eq!(call_i64_i32(c, "wrap", 0x1_0000_0000_i64)?, 0);
        assert_eq!(call_i64_i32(c, "wrap", 0xFFFF_FFFF_i64)?, -1);
        assert_eq!(call_i64_i32(c, "wrap", 0x1_FFFF_FFFF_i64)?, -1);
        assert_eq!(call_i64_i32(c, "wrap", i64::MAX)?, -1);
        assert_eq!(call_i64_i32(c, "wrap", i64::MIN)?, 0);

        // i64.extend_i32_s
        assert_eq!(call_i32_i64(c, "ext_i32_s", 0)?, 0);
        assert_eq!(call_i32_i64(c, "ext_i32_s", 1)?, 1);
        assert_eq!(call_i32_i64(c, "ext_i32_s", -1)?, -1);
        assert_eq!(call_i32_i64(c, "ext_i32_s", i32::MAX)?, i32::MAX as i64);
        assert_eq!(call_i32_i64(c, "ext_i32_s", i32::MIN)?, i32::MIN as i64);

        // i64.extend_i32_u
        assert_eq!(call_i32_i64(c, "ext_i32_u", 0)?, 0);
        assert_eq!(call_i32_i64(c, "ext_i32_u", 1)?, 1);
        assert_eq!(call_i32_i64(c, "ext_i32_u", -1)?, 0xFFFF_FFFF_i64);
        assert_eq!(call_i32_i64(c, "ext_i32_u", i32::MIN)?, 0x8000_0000_i64);

        // i32.trunc_f32_s — golden path
        assert_eq!(call_f32_i32(c, "trunc_f32_s", 0.0)?, 0);
        assert_eq!(call_f32_i32(c, "trunc_f32_s", 3.7)?, 3);
        assert_eq!(call_f32_i32(c, "trunc_f32_s", -3.7)?, -3);
        assert_eq!(call_f32_i32(c, "trunc_f32_s", 2147483520.0)?, 2147483520);
        // -2^31 representable exactly in f32
        assert_eq!(call_f32_i32(c, "trunc_f32_s", -2147483648.0)?, i32::MIN);

        // i32.trunc_f32_u — golden path
        assert_eq!(call_f32_i32(c, "trunc_f32_u", 0.0)?, 0);
        assert_eq!(call_f32_i32(c, "trunc_f32_u", 3.7)?, 3);
        // largest u32 representable in f32 below 2^32: 4294967040.0
        assert_eq!(call_f32_i32(c, "trunc_f32_u", 4294967040.0)?, -256_i32);

        // i32.trunc_f64_s/u
        assert_eq!(call_f64_i32(c, "trunc_f64_s", 0.0)?, 0);
        assert_eq!(call_f64_i32(c, "trunc_f64_s", 3.7)?, 3);
        assert_eq!(call_f64_i32(c, "trunc_f64_s", -3.7)?, -3);
        assert_eq!(call_f64_i32(c, "trunc_f64_s", 2147483647.0)?, i32::MAX);
        assert_eq!(call_f64_i32(c, "trunc_f64_s", -2147483648.0)?, i32::MIN);
        assert_eq!(call_f64_i32(c, "trunc_f64_u", 0.0)?, 0);
        assert_eq!(call_f64_i32(c, "trunc_f64_u", 4294967295.0)?, -1_i32);

        // i32.trunc_sat_* saturates rather than traps
        assert_eq!(call_f32_i32(c, "trunc_sat_f32_s", f32::NAN)?, 0);
        assert_eq!(call_f32_i32(c, "trunc_sat_f32_s", f32::INFINITY)?, i32::MAX);
        assert_eq!(
            call_f32_i32(c, "trunc_sat_f32_s", f32::NEG_INFINITY)?,
            i32::MIN
        );
        assert_eq!(call_f32_i32(c, "trunc_sat_f32_u", f32::NAN)?, 0);
        assert_eq!(call_f32_i32(c, "trunc_sat_f32_u", f32::INFINITY)?, -1_i32);
        assert_eq!(call_f32_i32(c, "trunc_sat_f32_u", f32::NEG_INFINITY)?, 0);
        assert_eq!(call_f64_i32(c, "trunc_sat_f64_s", f64::NAN)?, 0);
        assert_eq!(call_f64_i32(c, "trunc_sat_f64_s", 1e30)?, i32::MAX);
        assert_eq!(call_f64_i32(c, "trunc_sat_f64_s", -1e30)?, i32::MIN);
        assert_eq!(call_f64_i32(c, "trunc_sat_f64_u", f64::NAN)?, 0);
        assert_eq!(call_f64_i32(c, "trunc_sat_f64_u", 1e30)?, -1_i32);
        assert_eq!(call_f64_i32(c, "trunc_sat_f64_u", -1e30)?, 0);

        // i32.reinterpret_f32 / f32.reinterpret_i32 — round-trip
        let f1 = 1.0f32;
        let bits1 = 0x3F800000_u32 as i32;
        assert_eq!(call_f32_i32(c, "reint_f32", f1)?, bits1);
        assert_eq!(call_i32_f32(c, "reint_i32", bits1)?.to_bits(), f1.to_bits());
        // negative value, NaN-bit-pattern preservation
        let bits_neg = (-1.5f32).to_bits() as i32;
        assert_eq!(call_f32_i32(c, "reint_f32", -1.5f32)?, bits_neg);
        assert_eq!(
            call_i32_f32(c, "reint_i32", bits_neg)?.to_bits(),
            (-1.5f32).to_bits()
        );
        assert_eq!(call_f32_i32(c, "reint_f32", 0.0f32)?, 0);
        assert_eq!(call_i32_f32(c, "reint_i32", 0)?.to_bits(), 0u32);

        // f32.convert_i32_s/u  (results may round; pick exactly-representable values)
        assert_eq!(call_i32_f32(c, "conv_i32_s_f32", 0)?, 0.0);
        assert_eq!(call_i32_f32(c, "conv_i32_s_f32", -1)?, -1.0);
        assert_eq!(call_i32_f32(c, "conv_i32_s_f32", 16)?, 16.0);
        assert_eq!(call_i32_f32(c, "conv_i32_u_f32", 0)?, 0.0);
        // -1 as u32 = 4294967295; nearest representable f32 = 4294967296.0
        assert_eq!(call_i32_f32(c, "conv_i32_u_f32", -1)?, 4294967296.0f32);
        assert_eq!(call_i32_f32(c, "conv_i32_u_f32", 16)?, 16.0);

        // f64.convert_i32_s/u — every i32 is exact in f64
        assert_eq!(call_i32_f64(c, "conv_i32_s_f64", 0)?, 0.0);
        assert_eq!(call_i32_f64(c, "conv_i32_s_f64", -1)?, -1.0);
        assert_eq!(
            call_i32_f64(c, "conv_i32_s_f64", i32::MAX)?,
            i32::MAX as f64
        );
        assert_eq!(
            call_i32_f64(c, "conv_i32_s_f64", i32::MIN)?,
            i32::MIN as f64
        );
        assert_eq!(call_i32_f64(c, "conv_i32_u_f64", 0)?, 0.0);
        assert_eq!(call_i32_f64(c, "conv_i32_u_f64", -1)?, 4294967295.0);
        assert_eq!(call_i32_f64(c, "conv_i32_u_f64", i32::MIN)?, 2147483648.0);

        Ok(())
    }

    /// Trap-correctness assertions for `i32.trunc_f*_s/u`. NaN / out-of-range
    /// inputs must trap with the wasm-spec-mandated codes (matches AN-off).
    fn trap_assertions(c: &mut ConvInstance) {
        // NaN → BadConversionToInteger
        assert_trap(
            call_f32_i32(c, "trunc_f32_s", f32::NAN),
            wasmtime::Trap::BadConversionToInteger,
            "trunc_f32_s nan",
        );
        assert_trap(
            call_f32_i32(c, "trunc_f32_u", f32::NAN),
            wasmtime::Trap::BadConversionToInteger,
            "trunc_f32_u nan",
        );
        assert_trap(
            call_f64_i32(c, "trunc_f64_s", f64::NAN),
            wasmtime::Trap::BadConversionToInteger,
            "trunc_f64_s nan",
        );
        assert_trap(
            call_f64_i32(c, "trunc_f64_u", f64::NAN),
            wasmtime::Trap::BadConversionToInteger,
            "trunc_f64_u nan",
        );
        // Out-of-range → IntegerOverflow
        assert_trap(
            call_f32_i32(c, "trunc_f32_s", 2147483904.0),
            wasmtime::Trap::IntegerOverflow,
            "trunc_f32_s overflow",
        );
        assert_trap(
            call_f32_i32(c, "trunc_f32_s", -2147483904.0),
            wasmtime::Trap::IntegerOverflow,
            "trunc_f32_s underflow",
        );
        assert_trap(
            call_f32_i32(c, "trunc_f32_u", -1.0),
            wasmtime::Trap::IntegerOverflow,
            "trunc_f32_u negative",
        );
        assert_trap(
            call_f32_i32(c, "trunc_f32_u", 4294967296.0),
            wasmtime::Trap::IntegerOverflow,
            "trunc_f32_u overflow",
        );
        assert_trap(
            call_f64_i32(c, "trunc_f64_s", 2147483648.0),
            wasmtime::Trap::IntegerOverflow,
            "trunc_f64_s overflow",
        );
        assert_trap(
            call_f64_i32(c, "trunc_f64_u", -1.0),
            wasmtime::Trap::IntegerOverflow,
            "trunc_f64_u negative",
        );
        // Infinities
        assert_trap(
            call_f32_i32(c, "trunc_f32_s", f32::INFINITY),
            wasmtime::Trap::IntegerOverflow,
            "trunc_f32_s +inf",
        );
        assert_trap(
            call_f32_i32(c, "trunc_f32_s", f32::NEG_INFINITY),
            wasmtime::Trap::IntegerOverflow,
            "trunc_f32_s -inf",
        );
    }

    #[test]
    fn conversions_without_an() -> wasmtime::Result<()> {
        let mut c = make_inst(false, None)?;
        correctness_assertions(&mut c)?;
        trap_assertions(&mut c);
        Ok(())
    }

    // Float operators are refused wholesale under AN-encoding (v1 scope).
    // The conversions.wat module contains f32/f64 ops, so any attempt to
    // instantiate it under AN must fail with a message mentioning
    // floating-point. The AN-off counterpart above still exercises the
    // module end-to-end as a baseline.
    #[test]
    fn conversions_refused_under_an() {
        let err = match make_inst(true, None) {
            Ok(_) => panic!("expected float refusal under AN, got Ok"),
            Err(e) => e,
        };
        let s = format!("{err:#}");
        assert!(
            s.contains("AN-encoding") && s.contains("floating-point"),
            "error message did not mention float refusal: {s}",
        );
    }
}

// Integer cross-type conversions that survive the AN float refusal:
// `i32.extend8_s/16_s` (stays inside the encoding) and the i32 <-> i64
// conversions (`i32.wrap_i64`, `i64.extend_i32_s/u`). Unlike `conversions.wat`,
// this module instantiates and runs end-to-end with AN both on and off, and
// results must match.
mod int_conversions {
    use wasmtime::{Config, Engine, Module, Store};

    const CONV_WAT: &str = include_str!("../../an_encoding/int_conversions.wat");

    struct ConvInstance {
        store: Store<()>,
        instance: wasmtime::Instance,
    }

    fn make_config(an_on: bool, a: Option<u64>) -> Config {
        let mut config = Config::new();
        config.an_encoding(an_on);
        if let Some(a) = a {
            config.an_constant(a);
        }
        config
    }

    fn make_inst(an_on: bool, a: Option<u64>) -> wasmtime::Result<ConvInstance> {
        let engine = Engine::new(&make_config(an_on, a))?;
        let module = Module::new(&engine, CONV_WAT)?;
        let mut store = Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
        Ok(ConvInstance { store, instance })
    }

    fn call_i32_i32(c: &mut ConvInstance, name: &str, x: i32) -> wasmtime::Result<i32> {
        let f = c.instance.get_typed_func::<i32, i32>(&mut c.store, name)?;
        f.call(&mut c.store, x)
    }
    fn call_i32_i64(c: &mut ConvInstance, name: &str, x: i32) -> wasmtime::Result<i64> {
        let f = c.instance.get_typed_func::<i32, i64>(&mut c.store, name)?;
        f.call(&mut c.store, x)
    }
    fn call_i64_i32(c: &mut ConvInstance, name: &str, x: i64) -> wasmtime::Result<i32> {
        let f = c.instance.get_typed_func::<i64, i32>(&mut c.store, name)?;
        f.call(&mut c.store, x)
    }

    /// Run every conversion-correctness assertion. Same expected results under
    /// AN-on and AN-off.
    fn correctness_assertions(c: &mut ConvInstance) -> wasmtime::Result<()> {
        // i32.extend8_s
        assert_eq!(call_i32_i32(c, "ext8_s", 0)?, 0);
        assert_eq!(call_i32_i32(c, "ext8_s", 127)?, 127);
        assert_eq!(call_i32_i32(c, "ext8_s", 128)?, -128);
        assert_eq!(call_i32_i32(c, "ext8_s", 255)?, -1);
        assert_eq!(call_i32_i32(c, "ext8_s", 0x100)?, 0);
        assert_eq!(call_i32_i32(c, "ext8_s", 0x1ff)?, -1);
        assert_eq!(call_i32_i32(c, "ext8_s", -1)?, -1);

        // i32.extend16_s
        assert_eq!(call_i32_i32(c, "ext16_s", 0)?, 0);
        assert_eq!(call_i32_i32(c, "ext16_s", 32767)?, 32767);
        assert_eq!(call_i32_i32(c, "ext16_s", 32768)?, -32768);
        assert_eq!(call_i32_i32(c, "ext16_s", 0xffff)?, -1);
        assert_eq!(call_i32_i32(c, "ext16_s", 0x10000)?, 0);
        assert_eq!(call_i32_i32(c, "ext16_s", -1)?, -1);

        // i32.wrap_i64
        assert_eq!(call_i64_i32(c, "wrap", 0)?, 0);
        assert_eq!(call_i64_i32(c, "wrap", i32::MAX as i64)?, i32::MAX);
        assert_eq!(call_i64_i32(c, "wrap", -1_i64)?, -1);
        assert_eq!(call_i64_i32(c, "wrap", 0x1_0000_0000_i64)?, 0);
        assert_eq!(call_i64_i32(c, "wrap", 0xFFFF_FFFF_i64)?, -1);
        assert_eq!(call_i64_i32(c, "wrap", 0x1_FFFF_FFFF_i64)?, -1);
        assert_eq!(call_i64_i32(c, "wrap", i64::MAX)?, -1);
        assert_eq!(call_i64_i32(c, "wrap", i64::MIN)?, 0);

        // i64.extend_i32_s
        assert_eq!(call_i32_i64(c, "ext_i32_s", 0)?, 0);
        assert_eq!(call_i32_i64(c, "ext_i32_s", 1)?, 1);
        assert_eq!(call_i32_i64(c, "ext_i32_s", -1)?, -1);
        assert_eq!(call_i32_i64(c, "ext_i32_s", i32::MAX)?, i32::MAX as i64);
        assert_eq!(call_i32_i64(c, "ext_i32_s", i32::MIN)?, i32::MIN as i64);

        // i64.extend_i32_u
        assert_eq!(call_i32_i64(c, "ext_i32_u", 0)?, 0);
        assert_eq!(call_i32_i64(c, "ext_i32_u", 1)?, 1);
        assert_eq!(call_i32_i64(c, "ext_i32_u", -1)?, 0xFFFF_FFFF_i64);
        assert_eq!(call_i32_i64(c, "ext_i32_u", i32::MIN)?, 0x8000_0000_i64);

        Ok(())
    }

    #[test]
    fn int_conversions_without_an() -> wasmtime::Result<()> {
        let mut c = make_inst(false, None)?;
        correctness_assertions(&mut c)
    }

    #[test]
    fn int_conversions_with_an() -> wasmtime::Result<()> {
        let mut c = make_inst(true, None)?;
        correctness_assertions(&mut c)
    }

    #[test]
    fn int_conversions_with_various_an_constants() -> wasmtime::Result<()> {
        for a in [1u64, 7, 1009, 65521, 16_777_215] {
            let mut c = make_inst(true, Some(a))?;
            correctness_assertions(&mut c)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Dirty-driven shadow resync at host-call boundaries.
//
// The wasm→host trampoline brackets every host call with a cross-check
// (before) and a resync (after). The resync used to unconditionally
// re-encode every defined memory from raw bytes — O(memory) per host call,
// and a fault-detection hole: any raw/shadow divergence that appeared
// *during* the host call was silently "healed" into the shadow instead of
// trapping at the next boundary.
//
// The resync is dirty-driven instead:
// - `Memory::write` re-encodes exactly the written byte range immediately
//   (rounded outward to the containing 4-byte slots).
// - `Memory::data_mut` / `data_and_store_mut` hand out an untracked
//   whole-memory borrow, so they mark the memory whole-dirty; the next
//   boundary resync re-encodes that memory in full and clears the flag.
// - Memories that are neither written via `Memory::write` nor borrowed
//   mutably are not resynced at all, so divergences there now survive
//   until the next cross-check and trap.
//
// Semantics change pinned here: `Memory::write` *outside* a host call used
// to leave the shadow stale (→ `AnMemoryMismatch` at the next boundary);
// with the immediate range re-encode it is now a legitimate host write.
mod dirty_resync {
    use super::{expect_host_read_mismatch, make_config};
    use wasmtime::{AsContextMut, Caller, Engine, Extern, Linker, Module, Store};

    const TWO_CALLS_WAT: &str = r#"
        (module
            (import "env" "host" (func $host))
            (memory (export "m") 1)
            (func (export "f") (result i32)
                call $host
                call $host
                i32.const 0)
            (func (export "load") (param i32) (result i32)
                (i32.load (local.get 0))))
    "#;

    /// Instantiate `wat` (must import `env::host` as a 0-arg func) with
    /// AN-encoding on and constant `a`. `action` runs inside the *first*
    /// `host` call only; later calls are no-ops. The store data counts the
    /// host calls.
    fn setup(
        a: u64,
        wat: &str,
        multi_memory: bool,
        action: impl Fn(&mut Caller<'_, u32>) + Send + Sync + 'static,
    ) -> wasmtime::Result<(Store<u32>, wasmtime::Instance)> {
        let mut config = make_config(true);
        config.an_constant(a);
        if multi_memory {
            config.wasm_multi_memory(true);
        }
        let engine = Engine::new(&config)?;
        let module = Module::new(&engine, wat)?;
        let mut linker = Linker::new(&engine);
        linker.func_wrap("env", "host", move |mut caller: Caller<'_, u32>| {
            let n = *caller.data();
            *caller.data_mut() = n + 1;
            if n == 0 {
                action(&mut caller);
            }
        })?;
        let mut store = Store::new(&engine, 0u32);
        let instance = linker.instantiate(&mut store, &module)?;
        Ok((store, instance))
    }

    fn get_mem(caller: &mut Caller<'_, u32>, name: &str) -> wasmtime::Memory {
        match caller.get_export(name) {
            Some(Extern::Memory(m)) => m,
            other => panic!("export `{name}` is not a memory: {other:?}"),
        }
    }

    /// A shadow/raw divergence introduced *during* a host call (here: a shadow
    /// byte flip via the test accessor) is not silently healed — under
    /// verify-at-use it is caught when the affected slot is next read. The old
    /// unconditional full resync re-encoded the shadow from raw right after the
    /// host returned, erasing the divergence; the dirty-driven resync only
    /// touches `data_mut`-marked memories, so an untracked shadow flip survives.
    #[test]
    fn shadow_tamper_during_hostcall_detected_on_read() -> wasmtime::Result<()> {
        let (mut store, instance) = setup(65521, TWO_CALLS_WAT, false, |caller| {
            let m = get_mem(caller, "m");
            let shadow = m
                .an_shadow_data_mut_for_test(caller.as_context_mut())
                .expect("shadow allocated under AN");
            shadow[8] ^= 0x01;
        })?;
        let f = instance.get_typed_func::<(), i32>(&mut store, "f")?;
        // No host-boundary cross-check any more, so the call returns normally;
        // the divergence (shadow[8] = raw bytes [4, 8)) surfaces on read.
        f.call(&mut store, ())?;
        let m = instance.get_memory(&mut store, "m").expect("memory export");
        expect_host_read_mismatch(&m, &mut store, 4, "shadow tamper mid-hostcall");
        Ok(())
    }

    /// `Memory::write` during a host call must re-encode exactly the written
    /// range — not the whole memory. An unrelated shadow tamper elsewhere
    /// must therefore survive the write's resync and trap at the next
    /// boundary. This pins the precision of the `Memory::write` hook: a
    /// whole-memory resync (flag-based or unconditional) would heal the
    /// tamper and pass the second boundary.
    #[test]
    fn memory_write_does_not_heal_unrelated_tamper() -> wasmtime::Result<()> {
        let (mut store, instance) = setup(65521, TWO_CALLS_WAT, false, |caller| {
            let m = get_mem(caller, "m");
            // Tamper the shadow slot of raw bytes [512, 516).
            let shadow = m
                .an_shadow_data_mut_for_test(caller.as_context_mut())
                .expect("shadow allocated under AN");
            shadow[1024] ^= 0x01;
            // Legit host write far away: raw bytes [64, 68).
            m.write(&mut *caller, 64, &[0xAA, 0xBB, 0xCC, 0xDD])
                .expect("in-bounds write");
        })?;
        let f = instance.get_typed_func::<(), i32>(&mut store, "f")?;
        f.call(&mut store, ())?;
        // `Memory::write` re-encoded only its range [64, 68); the shadow tamper
        // at raw [512, 516) survives and is caught on read.
        let m = instance.get_memory(&mut store, "m").expect("memory export");
        expect_host_read_mismatch(&m, &mut store, 512, "tamper outside written range");
        Ok(())
    }

    /// `Memory::write` during a host call is a legitimate host write: the
    /// written range is re-encoded, the boundary check passes, and wasm
    /// reads the written value.
    #[test]
    fn memory_write_during_hostcall_resyncs_written_range() -> wasmtime::Result<()> {
        let (mut store, instance) = setup(65521, TWO_CALLS_WAT, false, |caller| {
            let m = get_mem(caller, "m");
            m.write(&mut *caller, 256, &0xDEAD_BEEFu32.to_le_bytes())
                .expect("in-bounds write");
        })?;
        let f = instance.get_typed_func::<(), i32>(&mut store, "f")?;
        assert_eq!(f.call(&mut store, ())?, 0);
        let load = instance.get_typed_func::<i32, i32>(&mut store, "load")?;
        assert_eq!(load.call(&mut store, 256)? as u32, 0xDEAD_BEEF);
        Ok(())
    }

    /// Unaligned `Memory::write` spanning slot boundaries: the resync rounds
    /// outward to whole 4-byte slots and re-encodes them from raw, so the
    /// neighbouring bytes inside the boundary slots stay consistent too.
    #[test]
    fn unaligned_memory_write_resyncs_boundary_slots() -> wasmtime::Result<()> {
        let (mut store, instance) = setup(65521, TWO_CALLS_WAT, false, |caller| {
            let m = get_mem(caller, "m");
            // 7 bytes at offset 65: touches slots 16..18 (bytes 64..72).
            m.write(&mut *caller, 65, &[1, 2, 3, 4, 5, 6, 7])
                .expect("in-bounds write");
        })?;
        let f = instance.get_typed_func::<(), i32>(&mut store, "f")?;
        assert_eq!(f.call(&mut store, ())?, 0);
        let load = instance.get_typed_func::<i32, i32>(&mut store, "load")?;
        assert_eq!(load.call(&mut store, 64)? as u32, 0x0302_0100);
        assert_eq!(load.call(&mut store, 68)? as u32, 0x0706_0504);
        Ok(())
    }

    #[test]
    fn multi_range_resync_preserves_untouched_byte_check() -> wasmtime::Result<()> {
        let (mut store, instance) = setup(65521, TWO_CALLS_WAT, false, |_| {})?;
        let memory = instance.get_memory(&mut store, "m").expect("memory export");

        // Raw bytes 64 and 66..68 are legitimate host writes. Byte 65 is not
        // dirty; changing it must still be detected rather than laundered by
        // the whole-slot re-encode.
        {
            let (raw, _, _, _) = memory.an_untracked_data_shadow_and_store_mut(&mut store);
            raw[64] = 0xAA;
            raw[65] = 0xBB;
            raw[66..68].copy_from_slice(&[0xCC, 0xDD]);
        }
        assert!(!wasmtime::_internal::an_resync_ranges(
            &memory,
            &mut store,
            &[64..65, 66..68],
        ));

        // Batch validation is atomic: failure above left the original zero
        // codeword untouched instead of partially re-encoding the slot.
        let shadow = memory
            .an_shadow_data_mut_for_test(&mut store)
            .expect("shadow allocated under AN");
        assert_eq!(&shadow[128..136], &[0; 8]);
        Ok(())
    }

    /// Semantics change: `Memory::write` *outside* any host call (embedder
    /// writing between wasm invocations) re-encodes immediately, so the
    /// next boundary check passes. Previously the shadow went stale and the
    /// first host-call boundary trapped.
    #[test]
    fn memory_write_outside_hostcall_does_not_trap() -> wasmtime::Result<()> {
        let (mut store, instance) = setup(65521, TWO_CALLS_WAT, false, |_| {})?;
        let memory = instance
            .get_memory(&mut store, "m")
            .expect("memory export missing");
        memory.write(&mut store, 128, &0x1122_3344u32.to_le_bytes())?;
        let f = instance.get_typed_func::<(), i32>(&mut store, "f")?;
        assert_eq!(f.call(&mut store, ())?, 0);
        let load = instance.get_typed_func::<i32, i32>(&mut store, "load")?;
        assert_eq!(load.call(&mut store, 128)? as u32, 0x1122_3344);
        Ok(())
    }

    /// Back-compat: writes through the untracked whole-memory borrow
    /// (`Memory::data_mut`) during a host call mark the memory whole-dirty;
    /// the boundary resync re-encodes it in full, so nothing traps and wasm
    /// reads the written value.
    #[test]
    fn data_mut_during_hostcall_resyncs_whole_memory() -> wasmtime::Result<()> {
        let (mut store, instance) = setup(65521, TWO_CALLS_WAT, false, |caller| {
            let m = get_mem(caller, "m");
            m.data_mut(caller.as_context_mut())[304..308].copy_from_slice(&[9, 8, 7, 6]);
        })?;
        let f = instance.get_typed_func::<(), i32>(&mut store, "f")?;
        assert_eq!(f.call(&mut store, ())?, 0);
        let load = instance.get_typed_func::<i32, i32>(&mut store, "load")?;
        assert_eq!(load.call(&mut store, 304)? as u32, 0x0607_0809);
        Ok(())
    }

    const TWO_MEMORIES_WAT: &str = r#"
        (module
            (import "env" "host" (func $host))
            (memory (export "m0") 1)
            (memory (export "m1") 1)
            (func (export "f") (result i32)
                call $host
                call $host
                i32.const 0))
    "#;

    /// Multi-memory isolation: a whole-dirty mark on m0 (via `data_mut`)
    /// must resync only m0. A shadow tamper on m1 in the same host call must
    /// survive m0's resync and trap at the next boundary. The old
    /// all-memories resync healed m1 and passed.
    #[test]
    fn data_mut_does_not_heal_other_memory_tamper() -> wasmtime::Result<()> {
        let (mut store, instance) = setup(65521, TWO_MEMORIES_WAT, true, |caller| {
            let m0 = get_mem(caller, "m0");
            let m1 = get_mem(caller, "m1");
            m0.data_mut(caller.as_context_mut())[40] = 0x42;
            let shadow1 = m1
                .an_shadow_data_mut_for_test(caller.as_context_mut())
                .expect("m1 shadow allocated under AN");
            shadow1[16] ^= 0x01;
        })?;
        let f = instance.get_typed_func::<(), i32>(&mut store, "f")?;
        f.call(&mut store, ())?;
        // m0 was whole-dirtied by `data_mut` and re-encoded by its own resync;
        // m1's shadow tamper (shadow[16] = raw bytes [8, 12)) is on a different
        // memory, untracked, and survives — caught on read of m1.
        let m1 = instance.get_memory(&mut store, "m1").expect("m1 export");
        expect_host_read_mismatch(&m1, &mut store, 8, "m1 tamper, m0 dirty");
        Ok(())
    }

    /// The dirty-driven behaviours hold for every legal shape of `A`.
    #[test]
    fn dirty_resync_various_an_constants() -> wasmtime::Result<()> {
        for &a in &[1u64, 7, 1009, 65521, 16_777_215] {
            // Heal-window: shadow tamper mid-hostcall traps at next boundary.
            let (mut store, instance) = setup(a, TWO_CALLS_WAT, false, |caller| {
                let m = get_mem(caller, "m");
                let shadow = m
                    .an_shadow_data_mut_for_test(caller.as_context_mut())
                    .expect("shadow allocated under AN");
                shadow[8] ^= 0x01;
            })?;
            let f = instance.get_typed_func::<(), i32>(&mut store, "f")?;
            f.call(&mut store, ())?;
            let m = instance.get_memory(&mut store, "m").expect("memory export");
            expect_host_read_mismatch(
                &m,
                &mut store,
                4,
                &format!("shadow tamper mid-hostcall, A={a}"),
            );

            // Legit `Memory::write` mid-hostcall round-trips.
            let (mut store, instance) = setup(a, TWO_CALLS_WAT, false, |caller| {
                let m = get_mem(caller, "m");
                m.write(&mut *caller, 256, &0xDEAD_BEEFu32.to_le_bytes())
                    .expect("in-bounds write");
            })?;
            let f = instance.get_typed_func::<(), i32>(&mut store, "f")?;
            assert_eq!(f.call(&mut store, ())?, 0, "A={a}");
            let load = instance.get_typed_func::<i32, i32>(&mut store, "load")?;
            assert_eq!(load.call(&mut store, 256)? as u32, 0xDEAD_BEEF, "A={a}");
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Review-fix regression tests: shadow-base reload after `memory.grow`,
// host-side `Memory::grow`, legitimate `data_mut` writes outside hostcalls,
// mixed-index `memory.copy` length decoding, SIMD/GC/exceptions/winch
// refusals, and imported-memory support.
// ---------------------------------------------------------------------------

const GROW_STORE_LOOP_WAT: &str = r#"
(module
  (import "env" "noop" (func $noop))
  (memory (export "m") 1 32)
  ;; Two stores straddling a `memory.grow` in one straight-line block: with a
  ;; `readonly`-flagged shadow-base load, GVN merges the second store's base
  ;; load with the first one's (which dominates it), so the second store
  ;; mirrors into the shadow buffer the grow just freed.
  (func (export "straddle") (result i32)
    (i32.store (i32.const 8) (i32.const 0x01020304))
    (drop (memory.grow (i32.const 1)))
    (i32.store (i32.const 16) (i32.const 0x0A0B0C0D))
    call $noop
    (i32.load (i32.const 16)))
  ;; Loop shape: grow + store repeatedly, then cross the boundary.
  (func (export "run") (result i32)
    (local $i i32)
    (loop $l
      (drop (memory.grow (i32.const 1)))
      (i32.store (i32.const 16) (i32.const 0x11223344))
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      (br_if $l (i32.lt_u (local.get $i) (i32.const 8))))
    call $noop
    (i32.load (i32.const 16))))
"#;

// Regression guard: the JIT load of the shadow base pointer used to be
// flagged `readonly`, which falsely asserts the slot never changes —
// `an_grow_shadow` re-allocates the shadow buffer and rewrites the slot on
// every successful `memory.grow`. In the current cranelift this was latent
// (load GVN/LICM additionally requires the `can_move` flag, which was never
// set), but any future optimizer change honoring `readonly` alone would have
// merged a dominating pre-grow load into post-grow stores: mirrors into
// freed memory. These shapes (straddling stores, grow+store loop) pin the
// reload-after-grow behavior.
#[test]
fn grow_then_store_same_function_reloads_shadow_base() -> wasmtime::Result<()> {
    let engine = Engine::new(&make_config(true))?;
    let module = Module::new(&engine, GROW_STORE_LOOP_WAT)?;
    let mut linker = Linker::new(&engine);
    linker.func_wrap("env", "noop", |_caller: wasmtime::Caller<'_, ()>| {})?;
    let mut store = Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &module)?;
    let straddle = instance.get_typed_func::<(), i32>(&mut store, "straddle")?;
    assert_eq!(straddle.call(&mut store, ())? as u32, 0x0A0B_0C0D);
    let run = instance.get_typed_func::<(), i32>(&mut store, "run")?;
    assert_eq!(run.call(&mut store, ())? as u32, 0x1122_3344);
    Ok(())
}

// Regression: the embedder-facing `Memory::grow` bypassed the shadow grow
// (only the wasm `memory.grow` libcall performed it), leaving a raw/shadow
// size mismatch. The `ld` read-back of a grown page exercises the load-side
// check against the grown shadow.
#[test]
fn host_memory_grow_keeps_shadow() -> wasmtime::Result<()> {
    let (mut store, memory, _grow, st, ld, _f) = grow_setup(65521)?;
    memory.grow(&mut store, 3)?;
    st.call(&mut store, (70_000, 0x5566_7788u32 as i32))?; // lands in a grown page
    assert_eq!(ld.call(&mut store, 70_000)? as u32, 0x5566_7788);
    Ok(())
}

// A host write through `Memory::data_mut` *outside* any host call is a
// legitimate untracked write: it marks the memory whole-dirty, and the
// wasm-entry heal re-encodes it from raw before guest code runs, so the
// guest load reads the written value instead of false-trapping with
// `AnMemoryMismatch` on a stale shadow.
#[test]
fn data_mut_outside_hostcall_does_not_trap() -> wasmtime::Result<()> {
    let (mut store, memory, _grow, _st, ld, _f) = grow_setup(65521)?;
    memory.data_mut(&mut store)[40] = 7;
    // `ld.call` enters wasm; the entry heal resyncs the whole-dirty memory
    // first, so the load sees the written byte and the load-side check passes.
    assert_eq!(ld.call(&mut store, 40)? & 0xff, 7);
    Ok(())
}

const MIXED_COPY_WAT: &str = r#"
(module
  (import "env" "noop" (func $noop))
  (memory $m32 (export "m32") 1)
  (memory $m64 (export "m64") i64 1)
  (func (export "fill32") (param i32 i32)
    (i32.store $m32 (local.get 0) (local.get 1)))
  (func (export "copy_to_64") (param i64 i32 i32)
    (memory.copy $m64 $m32 (local.get 0) (local.get 1) (local.get 2)))
  (func (export "load64") (param i64) (result i32)
    (i32.load $m64 (local.get 0)))
  (func (export "trigger") call $noop))
"#;

// `memory.copy` between a memory64 destination and a memory32 source has an
// i32-typed `len` (the *smaller* of the two index types per the wasm spec).
// Regression: the AN decode of `len` was gated on the destination's index
// type only, so the still-encoded length (`A*len`) reached the copy builtin.
#[test]
fn memory64_mixed_copy_len_decodes() -> wasmtime::Result<()> {
    let mut config = make_config(true);
    config.wasm_memory64(true);
    let engine = Engine::new(&config)?;
    let module = Module::new(&engine, MIXED_COPY_WAT)?;
    let mut linker = Linker::new(&engine);
    linker.func_wrap("env", "noop", |_caller: wasmtime::Caller<'_, ()>| {})?;
    let mut store = Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &module)?;
    let fill32 = instance.get_typed_func::<(i32, i32), ()>(&mut store, "fill32")?;
    let copy = instance.get_typed_func::<(i64, i32, i32), ()>(&mut store, "copy_to_64")?;
    let load64 = instance.get_typed_func::<i64, i32>(&mut store, "load64")?;
    fill32.call(&mut store, (0, 0xAABB_CCDDu32 as i32))?;
    copy.call(&mut store, (8, 0, 4))?;
    // The `load64` read-back verifies the copied range's shadow (load-side
    // check); a divergence would trap the load.
    assert_eq!(load64.call(&mut store, 8)? as u32, 0xAABB_CCDD);
    Ok(())
}

// SIMD is refused under AN by force-disabling the wasm feature: vector ops
// consume/produce wasm i32 values (shift counts, splats, extracts) that the
// AN translation does not cover, so the validator must reject such modules.
#[test]
fn simd_refused_under_an() {
    let wat = r#"
        (module
            (func (export "f") (result i32)
                (i32x4.extract_lane 0 (v128.const i32x4 1 2 3 4))))
    "#;
    let err =
        compile_with_config(&make_config(true), wat).expect_err("SIMD must be refused under AN");
    let s = format!("{err:#}").to_lowercase();
    assert!(s.contains("simd"), "error should mention SIMD: {s}");
}

// GC-proposal ops (`ref.i31` consumes a wasm i32; `i31.get_*` produce raw
// i32s the AN translation would not re-encode) are refused via the feature
// mask.
#[test]
fn gc_ops_refused_under_an() {
    let wat = r#"
        (module
            (func (export "f") (result i32)
                (i31.get_s (ref.i31 (i32.const 5)))))
    "#;
    compile_with_config(&make_config(true), wat).expect_err("GC ops must be refused under AN");
}

// Exception-handling ops carry i32 payloads with no AN translation; the
// feature mask keeps them disabled even if wasmtime ever enables the
// proposal by default.
#[test]
fn exceptions_refused_under_an() {
    let wat = r#"
        (module
            (tag $e (param i32))
            (func (export "f") (throw $e (i32.const 1))))
    "#;
    compile_with_config(&make_config(true), wat)
        .expect_err("exception ops must be refused under AN");
}

// Explicitly enabling a masked feature alongside AN is a configuration
// conflict surfaced at engine build, not a silent downgrade.
#[test]
fn explicit_simd_enable_conflicts_with_an() {
    let mut config = make_config(true);
    config.wasm_simd(true);
    let err = Engine::new(&config).expect_err("explicit SIMD + AN must be a config error");
    let s = format!("{err:#}");
    assert!(
        s.contains("AN-encoding"),
        "error should mention AN-encoding: {s}"
    );
}

// Component-model async (futures/streams) host-lowering only partially syncs
// the AN shadow and the full async path set is unaudited, so explicitly
// enabling it alongside AN is a configuration conflict at engine build rather
// than a partial guarantee.
#[test]
fn explicit_component_model_async_conflicts_with_an() {
    let mut config = make_config(true);
    config.wasm_component_model_async(true);
    let err = Engine::new(&config).expect_err("component-model-async + AN must be a config error");
    let s = format!("{err:#}");
    assert!(
        s.contains("AN-encoding"),
        "error should mention AN-encoding: {s}"
    );
}

// Winch has its own code generator that ignores the AN tunables; the
// combination is refused at engine build.
#[test]
fn winch_strategy_refused_under_an() {
    let mut config = make_config(true);
    config.strategy(wasmtime::Strategy::Winch);
    assert!(
        Engine::new(&config).is_err(),
        "winch + AN-encoding must be refused"
    );
}

// ---------------------------------------------------------------------------
// Imported (non-shared) memories under AN-encoding. The importing instance
// mirrors stores through the owning instance's shadow via the
// `VMMemoryImport::an_enc_base_slot` indirection, the boundary cross-check
// walks imported memories, and the bulk-memory ops re-encode through the
// owner.
// ---------------------------------------------------------------------------

const MEMORY_EXPORTER_WAT: &str = r#"
(module (memory (export "m") 1 16))
"#;

const MEMORY_IMPORTER_WAT: &str = r#"
(module
  (import "env" "m" (memory 1 16))
  (import "env" "noop" (func $noop))
  (func (export "poke") (param i32 i32)
    (i32.store (local.get 0) (local.get 1)))
  (func (export "peek") (param i32) (result i32)
    (i32.load (local.get 0)))
  (func (export "fill") (param i32 i32 i32)
    (memory.fill (local.get 0) (local.get 1) (local.get 2)))
  (func (export "copy") (param i32 i32 i32)
    (memory.copy (local.get 0) (local.get 1) (local.get 2)))
  (func (export "grow") (param i32) (result i32)
    (memory.grow (local.get 0)))
  (func (export "trigger") call $noop))
"#;

/// Instantiates an exporting module owning one memory plus an importing
/// module whose ops all target that import. Returns the store, the owner's
/// `Memory` handle, and the importing instance.
fn imported_memory_setup(
    a: u64,
) -> wasmtime::Result<(Store<()>, wasmtime::Memory, wasmtime::Instance)> {
    let mut config = make_config(true);
    config.an_constant(a);
    let engine = Engine::new(&config)?;
    let exporter = Module::new(&engine, MEMORY_EXPORTER_WAT)?;
    let importer = Module::new(&engine, MEMORY_IMPORTER_WAT)?;
    let mut store = Store::new(&engine, ());
    let exp_instance = wasmtime::Instance::new(&mut store, &exporter, &[])?;
    let memory = exp_instance
        .get_memory(&mut store, "m")
        .expect("exported memory");
    let mut linker = Linker::new(&engine);
    linker.define(&store, "env", "m", memory)?;
    linker.func_wrap("env", "noop", |_caller: wasmtime::Caller<'_, ()>| {})?;
    let imp_instance = linker.instantiate(&mut store, &importer)?;
    Ok((store, memory, imp_instance))
}

#[test]
fn imported_memory_stores_mirror_owner_shadow() -> wasmtime::Result<()> {
    let (mut store, memory, instance) = imported_memory_setup(65521)?;
    let poke = instance.get_typed_func::<(i32, i32), ()>(&mut store, "poke")?;
    let peek = instance.get_typed_func::<i32, i32>(&mut store, "peek")?;
    poke.call(&mut store, (8, 0x1234_5678))?;
    poke.call(&mut store, (13, 0x0102_0304))?; // unaligned, byte-RMW path
    // The `peek` guest loads below verify the importer sees the owner shadow
    // consistently (load-side check), and the host `memory.read` verifies the
    // owner-side view.
    assert_eq!(peek.call(&mut store, 8)?, 0x1234_5678);
    assert_eq!(peek.call(&mut store, 13)?, 0x0102_0304);
    // The owner-side host view sees the same raw bytes.
    let mut buf = [0u8; 4];
    memory.read(&store, 8, &mut buf)?;
    assert_eq!(u32::from_le_bytes(buf), 0x1234_5678);
    Ok(())
}

#[test]
fn imported_memory_tamper_raw_traps() -> wasmtime::Result<()> {
    let (mut store, memory, _instance) = imported_memory_setup(65521)?;
    tamper_raw_byte(&memory, &mut store, 3, |b| b ^ 0x80);
    // Host read of the owner memory (the import aliases its shadow) catches it.
    expect_host_read_mismatch(&memory, &mut store, 0, "imported memory raw bit flip");
    Ok(())
}

#[test]
fn imported_memory_tamper_shadow_traps() -> wasmtime::Result<()> {
    let (mut store, memory, _instance) = imported_memory_setup(65521)?;
    let shadow = memory
        .an_shadow_data_mut_for_test(&mut store)
        .expect("owner shadow allocated under AN");
    // shadow[8] = shadow slot 1 = raw bytes [4, 8); read offset 4.
    shadow[8] ^= 0x01;
    expect_host_read_mismatch(&memory, &mut store, 4, "imported memory shadow bit flip");
    Ok(())
}

#[test]
fn imported_memory_bulk_ops_keep_shadow() -> wasmtime::Result<()> {
    let (mut store, _memory, instance) = imported_memory_setup(65521)?;
    let poke = instance.get_typed_func::<(i32, i32), ()>(&mut store, "poke")?;
    let peek = instance.get_typed_func::<i32, i32>(&mut store, "peek")?;
    let fill = instance.get_typed_func::<(i32, i32, i32), ()>(&mut store, "fill")?;
    let copy = instance.get_typed_func::<(i32, i32, i32), ()>(&mut store, "copy")?;
    fill.call(&mut store, (64, 0xAB, 10))?; // unaligned tail
    assert_eq!(peek.call(&mut store, 64)? as u32, 0xABAB_ABAB);
    poke.call(&mut store, (128, 0x0BAD_F00Du32 as i32))?;
    copy.call(&mut store, (200, 128, 4))?;
    assert_eq!(peek.call(&mut store, 200)? as u32, 0x0BAD_F00D);
    Ok(())
}

#[test]
fn imported_memory_grow_through_importer() -> wasmtime::Result<()> {
    let (mut store, _memory, instance) = imported_memory_setup(65521)?;
    let poke = instance.get_typed_func::<(i32, i32), ()>(&mut store, "poke")?;
    let peek = instance.get_typed_func::<i32, i32>(&mut store, "peek")?;
    let grow = instance.get_typed_func::<i32, i32>(&mut store, "grow")?;
    poke.call(&mut store, (16, 0x5151_6262))?;
    assert_eq!(grow.call(&mut store, 1)?, 1, "grow should succeed");
    // The owner re-allocated its shadow; the importer must observe the new
    // base through the slot indirection for both old and new pages. The `peek`
    // loads below verify the shadow via the load-side check.
    poke.call(&mut store, (65_536 + 16, 0x7373_8484u32 as i32))?;
    assert_eq!(peek.call(&mut store, 16)?, 0x5151_6262);
    assert_eq!(peek.call(&mut store, 65_536 + 16)? as u32, 0x7373_8484);
    Ok(())
}

#[test]
fn imported_memory_various_an_constants() -> wasmtime::Result<()> {
    for &a in &[1u64, 7, 1009, 65521, 16_777_215] {
        let (mut store, memory, instance) = imported_memory_setup(a)?;
        let poke = instance.get_typed_func::<(i32, i32), ()>(&mut store, "poke")?;
        let peek = instance.get_typed_func::<i32, i32>(&mut store, "peek")?;
        poke.call(&mut store, (8, 0x1234_5678))?;
        assert_eq!(peek.call(&mut store, 8)?, 0x1234_5678, "A={a}");
        if a > 1 {
            tamper_raw_byte(&memory, &mut store, 3, |b| b ^ 0x80);
            expect_host_read_mismatch(&memory, &mut store, 0, &format!("imported raw flip, A={a}"));
        }
    }
    Ok(())
}

// A host-created `Memory::new` is backed by a synthetic instance that gets a
// shadow like any defined memory; importing it works the same way, and the
// tracked host-write paths (`Memory::write`, `data_mut`) stay consistent.
#[test]
fn host_created_memory_imported_under_an() -> wasmtime::Result<()> {
    let engine = Engine::new(&make_config(true))?;
    let importer = Module::new(&engine, MEMORY_IMPORTER_WAT)?;
    let mut store = Store::new(&engine, ());
    let memory = wasmtime::Memory::new(&mut store, wasmtime::MemoryType::new(1, Some(16)))?;
    let mut linker = Linker::new(&engine);
    linker.define(&store, "env", "m", memory)?;
    linker.func_wrap("env", "noop", |_caller: wasmtime::Caller<'_, ()>| {})?;
    let instance = linker.instantiate(&mut store, &importer)?;
    let poke = instance.get_typed_func::<(i32, i32), ()>(&mut store, "poke")?;
    let peek = instance.get_typed_func::<i32, i32>(&mut store, "peek")?;
    // Each `peek` guest load verifies the slot's shadow (load-side check); the
    // host-side `data_mut` write is healed at the next wasm entry before peek.
    poke.call(&mut store, (8, 0x0BAD_F00Du32 as i32))?;
    assert_eq!(peek.call(&mut store, 8)? as u32, 0x0BAD_F00D);
    memory.write(&mut store, 32, &[1, 2, 3, 4])?;
    assert_eq!(
        peek.call(&mut store, 32)? as u32,
        u32::from_le_bytes([1, 2, 3, 4])
    );
    memory.data_mut(&mut store)[48] = 9;
    assert_eq!(peek.call(&mut store, 48)? & 0xff, 9);
    Ok(())
}

// Component core modules pass through the same AN refusals as plain core
// modules (`build_component_artifacts` used to skip the validation entirely,
// so a float-containing component compiled under AN).
#[test]
fn component_core_module_float_refused_under_an() {
    use wasmtime::component::Component;
    let component_wat = r#"
        (component
            (core module $m
                (func (export "f") (param f32) (result f32) (local.get 0)))
            (core instance $i (instantiate $m)))
    "#;
    let mut config = make_config(true);
    config.wasm_component_model(true);
    let engine = Engine::new(&config).expect("engine builds");
    let err = match Component::new(&engine, component_wat) {
        Ok(_) => panic!("float-containing component core module must be refused under AN"),
        Err(e) => e,
    };
    let s = format!("{err:#}");
    assert!(
        s.contains("floating-point"),
        "error should mention the float refusal: {s}"
    );
}
