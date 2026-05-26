;; Cross-type conversion regression module for the AN-encoding tests. One
;; function per touched i32-related conversion operator. Both AN-on and AN-off
;; runs use this same module and must produce identical results. Loaded by
;; `tests/all/an_encoding.rs` via `include_str!`.
(module
    ;; ----- sign-extension (stays inside the AN encoding) -----
    (func (export "ext8_s") (param i32) (result i32)
        local.get 0 i32.extend8_s)
    (func (export "ext16_s") (param i32) (result i32)
        local.get 0 i32.extend16_s)

    ;; ----- i32 <-> i64 -----
    (func (export "wrap") (param i64) (result i32)
        local.get 0 i32.wrap_i64)
    (func (export "ext_i32_s") (param i32) (result i64)
        local.get 0 i64.extend_i32_s)
    (func (export "ext_i32_u") (param i32) (result i64)
        local.get 0 i64.extend_i32_u)

    ;; ----- i32 <-> floats (truncating) -----
    (func (export "trunc_f32_s") (param f32) (result i32)
        local.get 0 i32.trunc_f32_s)
    (func (export "trunc_f32_u") (param f32) (result i32)
        local.get 0 i32.trunc_f32_u)
    (func (export "trunc_f64_s") (param f64) (result i32)
        local.get 0 i32.trunc_f64_s)
    (func (export "trunc_f64_u") (param f64) (result i32)
        local.get 0 i32.trunc_f64_u)

    ;; ----- i32 <-> floats (saturating, no trap) -----
    (func (export "trunc_sat_f32_s") (param f32) (result i32)
        local.get 0 i32.trunc_sat_f32_s)
    (func (export "trunc_sat_f32_u") (param f32) (result i32)
        local.get 0 i32.trunc_sat_f32_u)
    (func (export "trunc_sat_f64_s") (param f64) (result i32)
        local.get 0 i32.trunc_sat_f64_s)
    (func (export "trunc_sat_f64_u") (param f64) (result i32)
        local.get 0 i32.trunc_sat_f64_u)

    ;; ----- reinterpret -----
    (func (export "reint_f32") (param f32) (result i32)
        local.get 0 i32.reinterpret_f32)
    (func (export "reint_i32") (param i32) (result f32)
        local.get 0 f32.reinterpret_i32)

    ;; ----- i32 -> float (converting) -----
    (func (export "conv_i32_s_f32") (param i32) (result f32)
        local.get 0 f32.convert_i32_s)
    (func (export "conv_i32_u_f32") (param i32) (result f32)
        local.get 0 f32.convert_i32_u)
    (func (export "conv_i32_s_f64") (param i32) (result f64)
        local.get 0 f64.convert_i32_s)
    (func (export "conv_i32_u_f64") (param i32) (result f64)
        local.get 0 f64.convert_i32_u)

    ;; ----- fault-injection harness: each func sources its i32 from an
    ;; i32.const (no trampoline-side i32 arg), so the conversion-site
    ;; codeword check is the first decode boundary. Used by
    ;; `conversions_codeword_check_traps_*` tests with
    ;; `Config::an_inject_conversion_fault(N)`.
    (func (export "trap_ext_i32_s") (result i64)
        i32.const 12345 i64.extend_i32_s)
    (func (export "trap_ext_i32_u") (result i64)
        i32.const 12345 i64.extend_i32_u)
    (func (export "trap_conv_f32_s") (result f32)
        i32.const 12345 f32.convert_i32_s)
    (func (export "trap_conv_f32_u") (result f32)
        i32.const 12345 f32.convert_i32_u)
    (func (export "trap_conv_f64_s") (result f64)
        i32.const 12345 f64.convert_i32_s)
    (func (export "trap_conv_f64_u") (result f64)
        i32.const 12345 f64.convert_i32_u)
    (func (export "trap_reint_i32") (result f32)
        i32.const 12345 f32.reinterpret_i32)
)
