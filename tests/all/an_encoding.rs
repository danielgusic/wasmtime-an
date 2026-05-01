use wasmtime::{Config, Engine, Module, Store};
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
    config.an_encoding_prototype(an_enabled);
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

fn check(backend: Backend) -> wasmtime::Result<()> {
    assert_eq!(run_mul(backend, false, 7, 6)?, 42);
    assert_eq!(run_mul(backend, false, 0, 123)?, 0);
    assert_eq!(run_mul(backend, false, -3, 4)?, -12);
    Ok(())
}

#[test]
fn mul_without_an() -> wasmtime::Result<()> {
    check(Backend::Pulley)
}

#[test]
fn mul_with_an_prototype() -> wasmtime::Result<()> {
    check(Backend::Pulley)
}

#[test]
fn mul_without_an_native() -> wasmtime::Result<()> {
    check(Backend::Native)
}

#[test]
fn mul_with_an_prototype_native() -> wasmtime::Result<()> {
    check(Backend::Native)
}
