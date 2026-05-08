;; Per-operator regression module for the AN-encoding tests. One function per
;; touched i32 operator. Both AN-on and AN-off runs use this same module and
;; must produce identical results. Loaded by `tests/all/an_encoding.rs` via
;; `include_str!`.
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
