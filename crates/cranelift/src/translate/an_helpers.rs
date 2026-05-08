//! Helpers for the AN-encoding paths in `code_translator.rs`.
//!
//! Stays-encoded `i32.mul` is implemented as:
//!
//! ```text
//!   P_lo = imul.i64(A*n, A*m)              // low 64 of A^2*n*m
//!   P_hi = umulhi.i64(A*n, A*m)            // high; P = (P_hi, P_lo) fits in <96 bits
//!   (Q_hi, Q_lo) = udiv_u128_by_u64_const(P_hi, P_lo, A)   // Q = A*n*m
//!   result       = umod_u128_by_u64_const_to_i64(Q_hi, Q_lo, A*2^32)
//! ```
//!
//! Both divisions use the algorithm 4 described in "Improved Division by Invariant Integers", Möller & Granlund, 2010,
//! specialized to a build-time constant divisor `d` that fits in `u64`. (In our case the A
//! constant)
//! The dividend's high half is first reduced mod `d` via the Cranelift-auto-lowered
//! `udiv.i64`-by-constant; the resulting `(r1, n_lo)` pair (with `r1 < d`) is
//! then handed to the 2-by-1 step.
//!
//! All emitted IR is plain `i64` arithmetic plus `umulhi.i64` and
//! `uadd_overflow.i64`. We never materialize an `i128` value, never compute a
//! 128*128-bit product, and never invoke `udiv` on anything wider than `i64`.

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{InstBuilder, Value, types::*};
use cranelift_frontend::FunctionBuilder;

/// Precompute the Möller-Granlund reciprocal for a normalized divisor
/// `d_norm` (top bit set).
///
/// `v = floor((2^128 - 1) / d_norm) - 2^64`, fits in `u64`.
fn precompute_recip(d_norm: u64) -> u64 {
    assert!(
        d_norm >= 1u64 << 63,
        "d_norm must be normalized (top bit set)"
    );
    let q = u128::MAX / (d_norm as u128);
    (q - (1u128 << 64)) as u64
}

/// Logical right shift of a `u128` value (held as `(n_hi, n_lo)`) by a build-time
/// constant `k`. Returns `(new_hi, new_lo)`.
fn ushr_pair(builder: &mut FunctionBuilder, n_hi: Value, n_lo: Value, k: u32) -> (Value, Value) {
    if k == 0 {
        return (n_hi, n_lo);
    }
    let zero = builder.ins().iconst(I64, 0);
    if k >= 128 {
        return (zero, zero);
    }
    if k == 64 {
        return (zero, n_hi);
    }
    if k > 64 {
        let lo = builder.ins().ushr_imm(n_hi, (k - 64) as i64);
        return (zero, lo);
    }
    let lo_part = builder.ins().ushr_imm(n_lo, k as i64);
    let hi_into_lo = builder.ins().ishl_imm(n_hi, (64 - k) as i64);
    let new_lo = builder.ins().bor(lo_part, hi_into_lo);
    let new_hi = builder.ins().ushr_imm(n_hi, k as i64);
    (new_hi, new_lo)
}

/// Logical left shift of a `u128` value (held as `(n_hi, n_lo)`) by a build-time
/// constant `k`. Returns `(new_hi, new_lo)`.
fn ishl_pair(builder: &mut FunctionBuilder, n_hi: Value, n_lo: Value, k: u32) -> (Value, Value) {
    if k == 0 {
        return (n_hi, n_lo);
    }
    let zero = builder.ins().iconst(I64, 0);
    if k >= 128 {
        return (zero, zero);
    }
    if k == 64 {
        return (n_lo, zero);
    }
    if k > 64 {
        let hi = builder.ins().ishl_imm(n_lo, (k - 64) as i64);
        return (hi, zero);
    }
    let new_lo = builder.ins().ishl_imm(n_lo, k as i64);
    let lo_into_hi = builder.ins().ushr_imm(n_lo, (64 - k) as i64);
    let hi_part = builder.ins().ishl_imm(n_hi, k as i64);
    let new_hi = builder.ins().bor(hi_part, lo_into_hi);
    (new_hi, new_lo)
}

/// Möller-Granlund division. Emits IR computing
/// `q = (u_hi * 2^64 + u_lo) / d` and `r = (u_hi * 2^64 + u_lo) mod d` as
/// `u64` values.
///
/// Runtime precondition: `u_hi < d`. The caller must guarantee this (we
/// achieve it by reducing the dividend's high half mod `d` first).
///
/// `d` is a build-time constant; we precompute the normalization shift `s`,
/// the normalized divisor `d_norm = d << s`, and the reciprocal `v`. The
/// dividend pair is shifted left by `s` at runtime to normalize.
fn div2by1_mg(builder: &mut FunctionBuilder, u_hi: Value, u_lo: Value, d: u64) -> (Value, Value) {
    debug_assert!(d > 1);
    debug_assert!(!d.is_power_of_two());

    let s = d.leading_zeros();
    let d_norm = d << s;
    let v = precompute_recip(d_norm);

    let (un_hi, un_lo) = ishl_pair(builder, u_hi, u_lo, s);

    let v_const = builder.ins().iconst(I64, v as i64);
    let d_norm_const = builder.ins().iconst(I64, d_norm as i64);

    // <q1, q0> = v * un_hi
    let q0_init = builder.ins().imul(v_const, un_hi);
    let q1_init = builder.ins().umulhi(v_const, un_hi);

    // <q1, q0> += <un_hi, un_lo>
    let (q0_full, c0) = builder.ins().uadd_overflow(q0_init, un_lo);
    let c0_64 = builder.ins().uextend(I64, c0);
    let q1_plus_un_hi = builder.ins().iadd(q1_init, un_hi);
    let q1_full = builder.ins().iadd(q1_plus_un_hi, c0_64);

    // q1 += 1 (overestimate)
    let q1_inc = builder.ins().iadd_imm(q1_full, 1);

    // r = un_lo - q1*d_norm  (low 64 bits, may wrap)
    let q1d = builder.ins().imul(q1_inc, d_norm_const);
    let r_init = builder.ins().isub(un_lo, q1d);

    // Adjust 1: if r > q0_full → q1 -= 1; r += d_norm.
    let r_gt_q0 = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThan, r_init, q0_full);
    let q1_dec = builder.ins().iadd_imm(q1_inc, -1);
    let q1_after1 = builder.ins().select(r_gt_q0, q1_dec, q1_inc);
    let r_added = builder.ins().iadd(r_init, d_norm_const);
    let r_after1 = builder.ins().select(r_gt_q0, r_added, r_init);

    // Adjust 2: if r >= d_norm → q1 += 1; r -= d_norm.
    let r_ge_d = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, r_after1, d_norm_const);
    let q1_inc2 = builder.ins().iadd_imm(q1_after1, 1);
    let q1_final = builder.ins().select(r_ge_d, q1_inc2, q1_after1);
    let r_sub = builder.ins().isub(r_after1, d_norm_const);
    let r_after2 = builder.ins().select(r_ge_d, r_sub, r_after1);

    // De-normalize the remainder. (Quotient does not need de-normalization
    // because (u*2^s)/(d*2^s) = u/d.)
    let r_final = if s == 0 {
        r_after2
    } else {
        builder.ins().ushr_imm(r_after2, s as i64)
    };

    (q1_final, r_final)
}

/// `u128` divided by a build-time `u64` constant. Returns `(q_hi, q_lo)`.
///
/// Step 1: reduce `n_hi` mod `d` via the Cranelift auto-lowered `udiv.i64`
/// (Granlund-Montgomery 64-bit reciprocal magic).
/// Step 2: hand `(r1, n_lo)` to [`div2by1_mg`] (which requires `r1 < d`,
/// trivially true since `r1 = n_hi mod d`).
pub(crate) fn udiv_u128_by_u64_const(
    builder: &mut FunctionBuilder,
    n_hi: Value,
    n_lo: Value,
    d: u64,
) -> (Value, Value) {
    if d == 1 {
        return (n_hi, n_lo);
    }
    if d.is_power_of_two() {
        return ushr_pair(builder, n_hi, n_lo, d.trailing_zeros());
    }

    let d_const = builder.ins().iconst(I64, d as i64);
    let q_hi = builder.ins().udiv(n_hi, d_const);
    let q_hi_d = builder.ins().imul(q_hi, d_const);
    let r1 = builder.ins().isub(n_hi, q_hi_d);

    let (q_lo, _r) = div2by1_mg(builder, r1, n_lo, d);
    (q_hi, q_lo)
}

/// `u128 mod u64-const` returning a `u64` (always fits, since `r < d ≤
/// u64::MAX`).
pub(crate) fn umod_u128_by_u64_const_to_i64(
    builder: &mut FunctionBuilder,
    n_hi: Value,
    n_lo: Value,
    d: u64,
) -> Value {
    if d == 1 {
        return builder.ins().iconst(I64, 0);
    }
    if d.is_power_of_two() {
        // For u64 d, k = log2(d) < 64, so the top half does not contribute.
        let k = d.trailing_zeros();
        debug_assert!(k < 64);
        let mask = (1u64 << k) - 1;
        let mask_const = builder.ins().iconst(I64, mask as i64);
        return builder.ins().band(n_lo, mask_const);
    }

    let d_const = builder.ins().iconst(I64, d as i64);
    let q_hi = builder.ins().udiv(n_hi, d_const);
    let q_hi_d = builder.ins().imul(q_hi, d_const);
    let r1 = builder.ins().isub(n_hi, q_hi_d);

    let (_q, r) = div2by1_mg(builder, r1, n_lo, d);
    r
}
