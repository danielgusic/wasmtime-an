//! Helpers for the AN-encoding paths in `code_translator.rs`.
//!
//! Two families of helpers live here:
//!
//! 1. **`i32.mul`**: stays-encoded multiply via Möller-Granlund 2-by-1 division
//!    (see below).
//! 2. **`i32.{and,or,xor}`**: tabulated bitwise logical ops following the
//!    Fetzer/Schiffel/Süßkraut paper. Each op uses an engine-owned 256×256
//!    `i32` table (`A * (c1 OP c2)`) generated in
//!    `crates/wasmtime/src/runtime/an_lut.rs`. `A` is constrained to
//!    `A < 2^23` so `A * 255 < 2^31` fits in `i32`. The table base pointer
//!    lives in a fixed `VMContext` slot; JIT'd code loads it via
//!    `vmctx + offset` so the same machine code is portable across processes
//!    (cwasm-friendly).
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

use crate::func_environ::FuncEnvironment;
use crate::translate::TargetEnvironment;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{self, InstBuilder, MemFlags, Value, types::*};
use cranelift_frontend::FunctionBuilder;
use wasmtime_environ::PtrSize;

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

/// Encoded i32 left shift (`shl`).
///
/// Computes `A * ((v << k) mod 2^32)` given `enc_v = A*v` (`v in [0, 2^32)`)
/// and a raw (unencoded) shift count `k_mod` in `[0, 32]`. The shift count
/// itself is *not* encoded — it must be decoded by the caller before being
/// passed in.
///
/// Method: `enc_v * 2^k = A*v*2^k`. Canonicalize modulo `A*2^32` using the
/// 128/64 Möller-Granlund helper:
///
/// ```text
///   two_k        = 1 << k_mod                 // unencoded 2^k
///   (P_hi, P_lo) = (umulhi, imul)(enc_v, two_k)
///   result       = umod_u128_by_u64_const_to_i64(P_hi, P_lo, A * 2^32)
/// ```
///
/// At `k_mod = 0` the multiplication is identity, mod is identity, result is
/// `enc_v`. At `k_mod = 32` the multiplication gives `enc_v * 2^32 =
/// A*v*2^32`, which is `0 mod (A*2^32)` (matches wasm: shl-by-32 = shl-by-0
/// for `k_mod = k & 31`, but the [0, 32] support is useful for the rotation
/// helpers that pass `32 - k_mod`).
pub(crate) fn emit_an_shl_i32(
    builder: &mut FunctionBuilder,
    environ: &mut FuncEnvironment<'_>,
    enc_v: Value,
    k_mod: Value,
) -> Value {
    let a = environ.tunables().an_constant;
    let one = builder.ins().iconst(I64, 1);
    let two_k = builder.ins().ishl(one, k_mod);
    let p_lo = builder.ins().imul(enc_v, two_k);
    let p_hi = builder.ins().umulhi(enc_v, two_k);
    let aw = a << 32;
    umod_u128_by_u64_const_to_i64(builder, p_hi, p_lo, aw)
}

/// Encoded i32 logical right shift (`shr_u`).
///
/// Computes `A * (v >> k)` given `enc_v = A*v` and raw `k_mod` in `[0, 32]`.
///
/// Trick: dividing by `A * 2^k` cancels `A` out of the dividend:
///
/// ```text
///   shift_div = A << k_mod          // = A * 2^k
///   q         = enc_v udiv shift_div // = (A*v) / (A*2^k) = v >> k  (raw)
///   result    = q * A                // re-encode
/// ```
///
/// `A * 2^k` fits in `i64` because `A < 2^23` and `k_mod ≤ 32` so the product
/// is `< 2^55`. At `k_mod = 32`, `shift_div = A * 2^32 = aw`, and since
/// `enc_v < aw` the quotient is `0` — matches "right-shift by 32 of a 32-bit
/// value zeroes everything," useful for rotations.
pub(crate) fn emit_an_shr_u_i32(
    builder: &mut FunctionBuilder,
    environ: &mut FuncEnvironment<'_>,
    enc_v: Value,
    k_mod: Value,
) -> Value {
    let a = environ.tunables().an_constant;
    let a_const = builder.ins().iconst(I64, a as i64);
    let shift_div = builder.ins().ishl(a_const, k_mod);
    let q = builder.ins().udiv(enc_v, shift_div);
    builder.ins().imul(q, a_const)
}

/// Selector for the AN-encoding bitwise binary ops handled by
/// [`emit_an_bitwise_i32`]. Picks the matching `VMContext` slot to load the
/// table base from.
#[derive(Debug, Clone, Copy)]
pub(crate) enum AnBitwiseOp {
    And,
    Or,
    Xor,
}

impl AnBitwiseOp {
    fn vmctx_offset<P: PtrSize>(self, ptr: &P) -> u8 {
        match self {
            AnBitwiseOp::And => ptr.vmctx_an_and_table(),
            AnBitwiseOp::Or => ptr.vmctx_an_or_table(),
            AnBitwiseOp::Xor => ptr.vmctx_an_xor_table(),
        }
    }
}

/// Emit chunked-LUT bitwise binary op for AN-encoded i32 operands.
///
/// Inputs are encoded as `A * n` and `A * m` with `n, m ∈ [0, 2^32)`. The op
/// (`AND` / `OR` / `XOR`) is tabulated on functional 8-bit chunks: a
/// pre-built `i32` table stores
/// `tab[(c1 << 8) | c2] = A * (c1 OP c2)` for `c1, c2 ∈ [0, 255]`. The
/// `i32` element type is safe because `A < 2^23`, so
/// `A * 255 < 2^31` ≤ `i32::MAX`.
///
/// Sequence emitted:
///
/// ```text
///   base = load.i64 [vmctx + offset_of_<op>_table]
///   n = udiv(arg1, A)                       // single decode at op site
///   m = udiv(arg2, A)
///   acc = 0
///   for i in 0..4:
///       c1 = (n >> (8*i)) & 0xff
///       c2 = (m >> (8*i)) & 0xff
///       idx = (c1 << 8) | c2
///       e32 = load.i32 [base + idx*4]       // entry already encoded
///       e   = uextend.i64(e32)              // widen for the i64 accumulator
///       acc += e << (8*i)                   // recombine into A * (n OP m)
///   return acc
/// ```
///
/// The recombined sum is bounded by `A * (2^32 − 1) < A * 2^32 < 2^55`, so it
/// fits in an `i64` and matches the rest of the AN-encoding invariant.
///
/// The base pointer is loaded from the engines `VMContext` slot, so
/// no absolute address ends up baked into the machine code.
pub(crate) fn emit_an_bitwise_i32(
    builder: &mut FunctionBuilder,
    environ: &mut FuncEnvironment<'_>,
    op: AnBitwiseOp,
    arg1: Value,
    arg2: Value,
) -> Value {
    let a = environ.tunables().an_constant;
    debug_assert!(a >= 1, "AN constant must be positive");

    // Load the LUT base pointer from this op's VMContext slot. Single load per
    // op (could be hoisted by GVN if multiple ops share a basic block).
    let table_offset = op.vmctx_offset(&environ.offsets.ptr);
    let vmctx_gv = environ.vmctx(&mut builder.func);
    let vmctx_val = builder.ins().global_value(I64, vmctx_gv);
    let mut readonly_mem = MemFlags::trusted();
    readonly_mem.set_readonly();
    let base = builder.ins().load(
        I64,
        readonly_mem,
        vmctx_val,
        ir::immediates::Offset32::new(table_offset.into()),
    );

    // Decode once per operand, it is emitted as reciprocal multiply by cranelift automatically
    let a_const = builder.ins().iconst(I64, a as i64);
    let n = builder.ins().udiv(arg1, a_const);
    let m = builder.ins().udiv(arg2, a_const);

    let mut acc: Option<Value> = None;
    for i in 0..4u32 {
        let shift = (i * 8) as i64;
        // Extract chunk from each operand.
        let n_shifted = if shift == 0 {
            n
        } else {
            builder.ins().ushr_imm(n, shift)
        };
        let m_shifted = if shift == 0 {
            m
        } else {
            builder.ins().ushr_imm(m, shift)
        };
        let c1 = builder.ins().band_imm(n_shifted, 0xff);
        let c2 = builder.ins().band_imm(m_shifted, 0xff);

        // idx = (c1 << 8) | c2;  byte_offset = idx * 4 (i32 entries)
        let c1_hi = builder.ins().ishl_imm(c1, 8);
        let idx = builder.ins().bor(c1_hi, c2);
        let byte_off = builder.ins().ishl_imm(idx, 2);
        let entry_addr = builder.ins().iadd(base, byte_off);

        let entry_i32 = builder.ins().load(I32, readonly_mem, entry_addr, 0);
        // Widen to i64 for the accumulator. Entries are non-negative
        // (`A * (c1 OP c2)` ≥ 0 with `A > 0`), so zero-extend.
        let entry = builder.ins().uextend(I64, entry_i32);

        // Shift the encoded chunk-result into its place in the 32-bit value.
        let term = if shift == 0 {
            entry
        } else {
            builder.ins().ishl_imm(entry, shift)
        };

        acc = Some(match acc {
            None => term,
            Some(prev) => builder.ins().iadd(prev, term),
        });
    }

    acc.expect("loop runs four iterations")
}
