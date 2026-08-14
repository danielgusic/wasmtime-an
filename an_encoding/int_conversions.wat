;; Integer cross-type conversion module for the AN-encoding tests. Covers only
;; the conversions that are supported under AN-encoding: sign-extension that
;; stays inside the encoding (`i32.extend8_s/16_s`) and the i32 <-> i64
;; conversions (`i32.wrap_i64`, `i64.extend_i32_s/u`). Float conversions live in
;; `conversions.wat` and are refused wholesale under AN. Both AN-on and AN-off
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
)
