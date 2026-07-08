//! Helper functions and structures for the translation.
use crate::translate::environ::TargetEnvironment;
use core::u32;
use cranelift_codegen::ir;
use cranelift_frontend::FunctionBuilder;
use wasmparser::{FuncValidator, WasmModuleResources};
use wasmtime_environ::WasmResult;

/// Get the parameter and result types for the given Wasm blocktype.
pub fn blocktype_params_results<'a, T>(
    validator: &'a FuncValidator<T>,
    ty: wasmparser::BlockType,
) -> WasmResult<(
    impl ExactSizeIterator<Item = wasmparser::ValType> + Clone + 'a,
    impl ExactSizeIterator<Item = wasmparser::ValType> + Clone + 'a,
)>
where
    T: WasmModuleResources,
{
    return Ok(match ty {
        wasmparser::BlockType::Empty => (
            itertools::Either::Left(std::iter::empty()),
            itertools::Either::Left(None.into_iter()),
        ),
        wasmparser::BlockType::Type(ty) => (
            itertools::Either::Left(std::iter::empty()),
            itertools::Either::Left(Some(ty).into_iter()),
        ),
        wasmparser::BlockType::FuncType(ty_index) => {
            let ty = validator
                .resources()
                .sub_type_at(ty_index)
                .expect("should be valid")
                .unwrap_func();

            (
                itertools::Either::Right(ty.params().iter().copied()),
                itertools::Either::Right(ty.results().iter().copied()),
            )
        }
    });
}

/// IR type carrying a wasm integer on the operand stack. Under AN-encoding
/// integers are widened (`i32` -> `I64`, `i64` -> `I128`) so the encoded value
/// `A*v` fits losslessly; otherwise the native width is used. Must agree with
/// `wasm_stack_value_type` (signatures) — locals, block params, and function
/// signatures all share the widened representation.
pub fn wasm_int_stack_type(an_encoding: bool, ty: wasmparser::ValType) -> ir::Type {
    match ty {
        wasmparser::ValType::I32 if an_encoding => ir::types::I64,
        wasmparser::ValType::I64 if an_encoding => ir::types::I128,
        wasmparser::ValType::I32 => ir::types::I32,
        wasmparser::ValType::I64 => ir::types::I64,
        _ => panic!("wasm_int_stack_type on non-integer type {ty:?}"),
    }
}

/// Create a `Block` with the given Wasm parameters.
pub fn block_with_params<PE: TargetEnvironment + ?Sized>(
    builder: &mut FunctionBuilder,
    params: impl IntoIterator<Item = wasmparser::ValType>,
    environ: &PE,
) -> WasmResult<ir::Block> {
    let block = builder.create_block();
    let an_encoding = environ.tunables().an_encoding;
    for ty in params {
        match ty {
            wasmparser::ValType::I32 | wasmparser::ValType::I64 => {
                builder.append_block_param(block, wasm_int_stack_type(an_encoding, ty));
            }
            wasmparser::ValType::F32 => {
                builder.append_block_param(block, ir::types::F32);
            }
            wasmparser::ValType::F64 => {
                builder.append_block_param(block, ir::types::F64);
            }
            wasmparser::ValType::Ref(rt) => {
                let hty = environ.convert_heap_type(rt.heap_type())?;
                let (ty, needs_stack_map) = environ.reference_type(hty);
                let val = builder.append_block_param(block, ty);
                if needs_stack_map {
                    builder.declare_value_needs_stack_map(val);
                }
            }
            wasmparser::ValType::V128 => {
                builder.append_block_param(block, ir::types::I8X16);
            }
        }
    }
    Ok(block)
}

/// Turns a `wasmparser` `f32` into a `Cranelift` one.
pub fn f32_translation(x: wasmparser::Ieee32) -> ir::immediates::Ieee32 {
    ir::immediates::Ieee32::with_bits(x.bits())
}

/// Turns a `wasmparser` `f64` into a `Cranelift` one.
pub fn f64_translation(x: wasmparser::Ieee64) -> ir::immediates::Ieee64 {
    ir::immediates::Ieee64::with_bits(x.bits())
}

/// Special VMContext value label. It is tracked as 0xffff_fffe label.
pub fn get_vmctx_value_label() -> ir::ValueLabel {
    const VMCTX_LABEL: u32 = 0xffff_fffe;
    ir::ValueLabel::from_u32(VMCTX_LABEL)
}
