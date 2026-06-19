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
//!   P_hi = umulhi.i64(A*n, A*m)            // high; P = (P_hi, P_lo) < 2^110 (A < 2^23)
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
use wasmtime_environ::{MemoryIndex, PtrSize};

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
    // Codeword check on the value operand before this decoding divide.
    emit_an_codeword_validity_check(builder, a, enc_v);
    let a_const = builder.ins().iconst(I64, a as i64);
    let shift_div = builder.ins().ishl(a_const, k_mod);
    let q = builder.ins().udiv(enc_v, shift_div);
    builder.ins().imul(q, a_const)
}

/// Load the base pointer of the AN-encoding shadow for a wasm memory.
///
/// The returned `I64` value is the runtime address of the first byte of the
/// shadow buffer for `memory_index`. For a defined memory it is read from
/// the per-memory `VMContext` slot written by `Instance::set_an_enc_shadows`;
/// for an imported memory it is read through one extra indirection — the
/// `VMMemoryImport::an_enc_base_slot` pointer into the *owning* instance's
/// slot. JIT'd store code adds an in-shadow offset
/// (`raw_addr * ENC_MEM_GROWTH_FACTOR` plus an in-slot byte position) and
/// writes the encoded value.
///
/// Returns `None` only when AN-encoding is off. Callers must skip the
/// shadow write in that case.
pub(crate) fn emit_an_enc_base_pointer(
    builder: &mut FunctionBuilder,
    environ: &mut FuncEnvironment<'_>,
    memory_index: MemoryIndex,
) -> Option<Value> {
    if !environ.tunables().an_encoding {
        return None;
    }

    let vmctx_gv = environ.vmctx(&mut builder.func);
    let vmctx_val = builder.ins().global_value(I64, vmctx_gv);

    let Some(def_idx) = environ.module.defined_memory_index(memory_index) else {
        // Imported memory: the shadow lives on the owning instance.
        // `VMMemoryImport::an_enc_base_slot` points at the owner's enc-base
        // slot; that pointer is written once at instantiation and never
        // changes, so loading it is `readonly`. The slot's *contents* (the
        // shadow base) change whenever the owner's memory grows, so the
        // second load must NOT be readonly (see the defined-memory case
        // below for why).
        //
        // Under AN-encoding every non-shared memory has a shadow (shared
        // memories are refused at compile time), so the slot pointer is
        // never null here.
        let import_off = environ.offsets.vmctx_vmmemory_import(memory_index)
            + u32::from(environ.offsets.vmmemory_import_an_enc_base_slot());
        let import_off = i32::try_from(import_off)
            .expect("VMContext offset for AN enc-base slot pointer does not fit in i32");
        let mut readonly_mem = MemFlags::trusted();
        readonly_mem.set_readonly();
        let slot_ptr = builder.ins().load(
            I64,
            readonly_mem,
            vmctx_val,
            ir::immediates::Offset32::new(import_off),
        );
        return Some(builder.ins().load(I64, MemFlags::trusted(), slot_ptr, 0));
    };

    let offset_u32 = environ.offsets.vmctx_an_enc_memory_base(def_idx);
    let offset_i32 = i32::try_from(offset_u32)
        .expect("VMContext offset for AN enc-memory base does not fit in i32");

    // NOT readonly: `an_grow_shadow` re-allocates the shadow buffer and
    // rewrites this slot on every successful `memory.grow`, so the load must
    // be re-issued after any call that may grow memory. A `readonly` flag
    // here would let GVN/LICM reuse a stale base pointer across a grow,
    // turning subsequent shadow stores into writes to freed memory.
    Some(builder.ins().load(
        I64,
        MemFlags::trusted(),
        vmctx_val,
        ir::immediates::Offset32::new(offset_i32),
    ))
}

/// Emit an AN-encoding read-modify-write of one shadow slot that touches
/// exactly *one* byte. Used by the byte-by-byte decomposition path for
/// cross-slot stores (`i32.store16` at `byte_pos == 3`, and unaligned
/// `i32.store`).
///
/// `byte_addr_i64` is the wasm-level byte address of the target byte.
/// `raw_byte_value_i64` is the byte to write, in the low 8 bits of an
/// `I64`; higher bits are masked off here so the caller can pass an
/// already-shifted value extracted from a wider operand.
pub(crate) fn emit_an_byte_store_rmw(
    builder: &mut FunctionBuilder,
    environ: &mut FuncEnvironment<'_>,
    enc_base: Value,
    byte_addr_i64: Value,
    raw_byte_value_i64: Value,
) {
    let a = environ.tunables().an_constant;
    let a_const = builder.ins().iconst(I64, a as i64);

    let slot_idx = builder.ins().ushr_imm(byte_addr_i64, 2);
    let slot_byte_off = builder.ins().ishl_imm(slot_idx, 3);
    let enc_slot_addr = builder.ins().iadd(enc_base, slot_byte_off);
    let byte_pos = builder.ins().band_imm(byte_addr_i64, 3);
    let shift_bits = builder.ins().ishl_imm(byte_pos, 3);

    let byte_mask = builder.ins().iconst(I64, 0xff);
    let byte_value_low = builder.ins().band(raw_byte_value_i64, byte_mask);
    let byte_value_shifted = builder.ins().ishl(byte_value_low, shift_bits);
    let slot_mask = builder.ins().ishl(byte_mask, shift_bits);
    let slot_keep_mask = builder.ins().bnot(slot_mask);

    let mem_flags = MemFlags::trusted();
    let old_enc = builder.ins().load(I64, mem_flags, enc_slot_addr, 0);
    let old_raw = builder.ins().udiv(old_enc, a_const);
    let raw_cleared = builder.ins().band(old_raw, slot_keep_mask);
    let new_raw_unmasked = builder.ins().bor(raw_cleared, byte_value_shifted);
    let low32_mask = builder.ins().iconst(I64, 0xffff_ffffu64 as i64);
    let new_raw = builder.ins().band(new_raw_unmasked, low32_mask);
    let new_enc = builder.ins().imul(new_raw, a_const);
    builder.ins().store(mem_flags, new_enc, enc_slot_addr, 0);
}

/// Emit `n` byte-RMW operations that mirror an `n`-byte store
/// (`n in [1, 4]`) into the shadow, regardless of alignment.
///
/// Byte `i` of `raw_value_i64` (from low to high) is written to wasm byte
/// address `base_addr_i64 + i`. Decoded once; each byte then shifted out
/// and passed to `emit_an_byte_store_rmw`. Cross-slot transitions handled
/// automatically by the per-byte slot computation inside the helper.
pub(crate) fn emit_an_multi_byte_decomposed_store(
    builder: &mut FunctionBuilder,
    environ: &mut FuncEnvironment<'_>,
    enc_base: Value,
    base_addr_i64: Value,
    raw_value_i64: Value,
    nbytes: u8,
) {
    debug_assert!(nbytes >= 1 && nbytes <= 4);
    for i in 0..nbytes {
        let byte_addr = if i == 0 {
            base_addr_i64
        } else {
            builder.ins().iadd_imm(base_addr_i64, i as i64)
        };
        let shifted = if i == 0 {
            raw_value_i64
        } else {
            builder.ins().ushr_imm(raw_value_i64, (i as i64) * 8)
        };
        emit_an_byte_store_rmw(builder, environ, enc_base, byte_addr, shifted);
    }
}

/// Convert a wasm-level effective byte address (post bounds check, with
/// `memarg.offset` already folded in) into the corresponding shadow byte
/// offset.
///
/// The mapping is `enc_off = effective_addr << 1` (each 4-byte raw slot
/// occupies 8 shadow bytes). Caller chooses the in-slot byte position based
/// on `effective_addr & 3` separately.
///
/// `effective_addr_val` may be either `I32` (memory32) or `I64` (memory64);
/// both are unsigned-extended to `I64` before shifting.
pub(crate) fn emit_an_enc_offset_from_effective_addr(
    builder: &mut FunctionBuilder,
    effective_addr_val: Value,
) -> Value {
    let ty = builder.func.dfg.value_type(effective_addr_val);
    let i64_addr = if ty == I64 {
        effective_addr_val
    } else {
        builder.ins().uextend(I64, effective_addr_val)
    };
    builder.ins().ishl_imm(i64_addr, 1)
}

/// Emit an inline AN-encoding load-side validity check covering every shadow
/// slot touched by an i32 load family op.
///
/// For each touched slot the check asserts:
/// `enc_slot == A * u32_le(raw_slot)`. Any mismatch raises
/// [`crate::TRAP_AN_MEMORY_MISMATCH`] immediately, at the load site. This is
/// the guest-read half of verify-at-use and is mandatory under AN — it is the
/// sole guard on guest reads, with no host-boundary cross-check behind it.
///
/// `raw_base_addr_i64` is the address `prepare_addr` produced for the load
/// (raw heap base + effective wasm byte address, with `memarg.offset`
/// folded in). `effective_addr_i64` is `wasm_index + memarg.offset` as an
/// `I64`; it's used only to derive in-slot byte positions and shadow slot
/// indices, never for raw memory access.
///
/// `nbytes` is the load's wasm-level access width in `{1, 2, 4}`. For
/// `nbytes >= 2` we always check both the slot containing the first byte
/// AND the slot containing the last byte; if those happen to coincide
/// (in-slot access) the second check is redundant but still correct.
/// Reading the slot of the last byte is safe under page-aligned memory
/// sizes (≥ 4 bytes per page) because `bounds-checked last_byte <
/// raw_size` and the slot read stays within `[slot_aligned_last_byte,
/// slot_aligned_last_byte + 4) ⊆ [0, raw_size]`.
pub(crate) fn emit_an_load_validity_check(
    builder: &mut FunctionBuilder,
    environ: &mut FuncEnvironment<'_>,
    enc_base: Value,
    raw_base_addr_i64: Value,
    effective_addr_i64: Value,
    nbytes: u8,
) {
    debug_assert!(matches!(nbytes, 1 | 2 | 4));
    let a = environ.tunables().an_constant;
    let a_const = builder.ins().iconst(I64, a as i64);

    // Slot 1: containing the first touched byte.
    check_one_slot_validity(
        builder,
        enc_base,
        raw_base_addr_i64,
        effective_addr_i64,
        a_const,
    );

    // Slot 2: containing the last touched byte. Identical to slot 1 when the
    // access fits in a single slot.
    if nbytes > 1 {
        let n_minus_1 = (nbytes - 1) as i64;
        let raw_last = builder.ins().iadd_imm(raw_base_addr_i64, n_minus_1);
        let eff_last = builder.ins().iadd_imm(effective_addr_i64, n_minus_1);
        check_one_slot_validity(builder, enc_base, raw_last, eff_last, a_const);
    }
}

fn check_one_slot_validity(
    builder: &mut FunctionBuilder,
    enc_base: Value,
    raw_byte_addr: Value,
    eff_byte_addr_i64: Value,
    a_const: Value,
) {
    // shadow_slot_addr = enc_base + (eff_byte_addr & ~3) << 1
    //   (each 4-byte raw slot → 8-byte shadow slot, factor 2)
    let eff_slot_aligned = builder.ins().band_imm(eff_byte_addr_i64, !3i64);
    let shadow_off = builder.ins().ishl_imm(eff_slot_aligned, 1);
    let shadow_slot_addr = builder.ins().iadd(enc_base, shadow_off);

    // raw_slot_addr = raw_byte_addr & ~3 (4-byte aligned)
    let raw_slot_addr = builder.ins().band_imm(raw_byte_addr, !3i64);

    let mem_flags = MemFlags::trusted();
    let enc_slot = builder.ins().load(I64, mem_flags, shadow_slot_addr, 0);
    let raw_slot_i32 = builder.ins().load(I32, mem_flags, raw_slot_addr, 0);
    let raw_slot_u64 = builder.ins().uextend(I64, raw_slot_i32);
    let expected = builder.ins().imul(raw_slot_u64, a_const);

    let mismatch = builder.ins().icmp(IntCC::NotEqual, enc_slot, expected);
    builder.ins().trapnz(mismatch, crate::TRAP_AN_MEMORY_MISMATCH);
}

/// Emit a boundary-side AN codeword validity check.
///
/// At every wasm/host trampoline boundary where an encoded i32 value
/// (widened `I64` holding `A*v`) is about to be decoded into a raw i32,
/// assert that the value is in fact a valid codeword: `val % A == 0`.
///
/// A non-zero remainder indicates external corruption (bit flip in a
/// register, cosmic ray, hardware fault) or an internal AN translation
/// bug that produced a non-multiple-of-`A` on the operand stack.
///
/// Lowered by cranelift to a reciprocal-multiply for `urem` by a
/// build-time constant, so the cost is a handful of instructions plus a
/// trapnz. Skipped at `A == 1` since every i64 is trivially a multiple of
/// 1.
/// Takes the AN constant `A` directly so it works from contexts that
/// don't carry a `FuncEnvironment` (notably the wasm/host trampolines in
/// `compiler.rs`, which build their CLIF without going through the
/// wasm-to-CLIF path).
pub(crate) fn emit_an_codeword_validity_check(
    builder: &mut FunctionBuilder,
    a: u64,
    val_i64: Value,
) {
    if a == 1 {
        return;
    }
    let a_const = builder.ins().iconst(I64, a as i64);
    let r = builder.ins().urem(val_i64, a_const);
    builder.ins().trapnz(r, crate::TRAP_AN_CODEWORD_INVALID);
}

/// Conversion-boundary decode of an AN-encoded i32: optionally inject a
/// fault offset (test-only `tunables.an_inject_conversion_fault`), assert
/// codeword validity (`val % A == 0`), then decode to a raw `I32`.
///
/// Use this at every cross-type conversion op that hands an encoded i32 to
/// a non-i32 sink (`i64.extend_i32_s/u`). For ops that keep the i32 inside
/// the encoding (e.g. `i32.extend8_s/16_s`), use the plain `udiv`/`ireduce`
/// pattern from `clz`/`ctz`/`popcnt` — no check needed since the invariant
/// is structurally maintained. (Float conversions are refused wholesale, so
/// they never reach this helper.)
pub(crate) fn emit_an_conversion_decode_i32(
    builder: &mut FunctionBuilder,
    environ: &mut FuncEnvironment<'_>,
    enc_v: Value,
) -> Value {
    let a = environ.tunables().an_constant;
    let fault = environ.tunables().an_inject_conversion_fault;
    let bumped = if fault != 0 {
        builder.ins().iadd_imm(enc_v, fault as i64)
    } else {
        enc_v
    };
    emit_an_codeword_validity_check(builder, a, bumped);
    let a_const = builder.ins().iconst(I64, a as i64);
    let raw_i64 = builder.ins().udiv(bumped, a_const);
    builder.ins().ireduce(I32, raw_i64)
}

/// Encode a wasm-`i32` boolean (Cranelift `I8` result of `icmp`/`fcmp`/
/// `vany_true`/`vall_true`/`ref.is_null`/…) into the wasm-`i32` slot on the
/// operand stack.
///
/// Under AN: encode as `select(bool, A, 0)` — same shape as the `i32.eqz`
/// path, producing a canonical encoded `I64` in `{0, A}`.
///
/// Under non-AN: zero-extend to `I32` (matches the long-standing default
/// path that callers used before this helper existed).
pub(crate) fn encode_wasm_i32_bool(
    builder: &mut FunctionBuilder,
    environ: &mut FuncEnvironment<'_>,
    bool_val: Value,
) -> Value {
    if environ.tunables().an_encoding {
        let a = environ.tunables().an_constant;
        let a_const = builder.ins().iconst(I64, a as i64);
        let zero = builder.ins().iconst(I64, 0);
        builder.ins().select(bool_val, a_const, zero)
    } else {
        builder.ins().uextend(I32, bool_val)
    }
}

/// Encode a raw `I32` value into the wasm-`i32` slot on the operand stack.
/// Used by SIMD ops (`i*x*.bitmask`) that produce a raw `I32` extract,
/// which under AN must be widened to `I64` and multiplied by `A` to match
/// the canonical operand-stack form. Under non-AN the value passes through.
pub(crate) fn encode_wasm_i32_raw(
    builder: &mut FunctionBuilder,
    environ: &mut FuncEnvironment<'_>,
    raw_i32: Value,
) -> Value {
    if environ.tunables().an_encoding {
        emit_an_encode_raw_i32(builder, environ, raw_i32)
    } else {
        raw_i32
    }
}

/// Encode a raw `I32` into the canonical AN-encoded `I64` form `A * v`
/// with `v in [0, 2^32)`.
///
/// Uses `uextend` to treat the i32 as unsigned (so e.g. `-1_i32` becomes
/// `0xFFFFFFFF_i64`), then multiplies by `A`. The product fits in `i64`
/// because `A < 2^23` and `uextend(i32) < 2^32`.
pub(crate) fn emit_an_encode_raw_i32(
    builder: &mut FunctionBuilder,
    environ: &mut FuncEnvironment<'_>,
    raw_i32: Value,
) -> Value {
    let a = environ.tunables().an_constant;
    let a_const = builder.ins().iconst(I64, a as i64);
    let zext = builder.ins().uextend(I64, raw_i32);
    builder.ins().imul(zext, a_const)
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

    // Codeword check on both operands before the decoding divides.
    emit_an_codeword_validity_check(builder, a, arg1);
    emit_an_codeword_validity_check(builder, a, arg2);
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
