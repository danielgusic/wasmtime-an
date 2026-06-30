//! AN-encoding bitwise lookup tables, generated in-process per engine.
//!
//! For each binary bitwise op supported under AN-encoding (`AND`, `OR`,
//! `XOR`), one 256×256 `i32` table is built whose entries hold the encoded
//! result of operating on functional 8-bit chunks:
//!
//! ```text
//!   tab[(c1 << 8) | c2] = A * (c1 OP c2)        with c1, c2 ∈ [0, 255]
//! ```
//!
//! `A` is constrained to `1 ≤ A < 2^23` so that the largest entry
//! `A * 255 < 2^31` fits in a signed `i32`. This halves table memory
//! (256 KiB per op instead of 512 KiB) at the cost of one `uextend.i64`
//! per lookup in JIT'd code.
//!
//! Tables are owned by [`crate::engine::EngineInner`] and addressed from
//! JIT'd code via fixed `VMContext` slots written at instance init (see
//! `Instance::set_an_lut_pointers`). Generation runs in `Engine::new` and is
//! negligible per op, so we don't bother with saving on disk.

use crate::prelude::*;
use core::fmt;

/// Number of entries in a binary 8-bit-chunk table. Indexed by
/// `(c1 << 8) | c2`.
pub(crate) const TABLE_LEN: usize = 256 * 256;

/// Upper bound (exclusive) for the AN constant `A`. Chosen so that
/// `A * 255 < 2^31` and table entries fit in a signed `i32`.
pub(crate) const A_MAX_EXCLUSIVE: u64 = 1u64 << 23;

/// Owning handle for one populated table. Boxed so the address is stable and
/// the buffer can be referenced by JIT-emitted code via the per-instance
/// `VMContext` slot without further indirection.
pub(crate) type Table = Box<[i32; TABLE_LEN]>;

/// Errors raised while validating `A` or generating tables. Surfaced from
/// `Engine::new` as a normal `wasmtime::Error`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AnLutError {
    /// `A` outside the supported range `1 ≤ A < 2^23`.
    InvalidA(u64),
}

impl fmt::Display for AnLutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AnLutError::InvalidA(a) => {
                write!(f, "invalid AN constant {a}: require 1 <= A < 2^23")
            }
        }
    }
}

impl core::error::Error for AnLutError {}

/// Validate `A` against the same bound the wasmtime config enforces:
/// `1 ≤ A < 2^23`.
pub(crate) fn validate_a(a: u64) -> Result<(), AnLutError> {
    if a == 0 || a >= A_MAX_EXCLUSIVE {
        return Err(AnLutError::InvalidA(a));
    }
    Ok(())
}

/// Bitwise binary operations covered by the LUT generator (the only ones in wasm).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BinOp {
    And,
    Or,
    Xor,
}

impl BinOp {
    fn apply(self, c1: u32, c2: u32) -> u32 {
        match self {
            BinOp::And => c1 & c2,
            BinOp::Or => c1 | c2,
            BinOp::Xor => c1 ^ c2,
        }
    }
}

/// Bundle of populated tables held by an `Engine` when AN-encoding is on.
pub(crate) struct AnLuts {
    pub(crate) and: Table,
    pub(crate) or: Table,
    pub(crate) xor: Table,
}

fn build_table(a: u64, op: BinOp) -> Table {
    // `validate_a` guarantees `a < 2^23`, so `a` fits in `i32`.
    let a_signed = a as i32;
    let mut buf: Box<[i32]> = vec![0i32; TABLE_LEN].into_boxed_slice();
    for c1 in 0u32..256 {
        let row = (c1 as usize) << 8;
        for c2 in 0u32..256 {
            // `op.apply(c1, c2) <= 255`, so `r` fits in `i32` and
            // `a_signed * r <= (2^23 - 1) * 255 < 2^31`, never overflows.
            let r = op.apply(c1, c2) as i32;
            buf[row | c2 as usize] = a_signed.wrapping_mul(r);
        }
    }
    buf.try_into()
        .expect("buffer length matches TABLE_LEN by construction")
}

/// Build all three AN-encoding bitwise tables for the given `A`. Validates
/// `A`; returns `Err` on out-of-range constants.
pub(crate) fn generate(a: u64) -> Result<AnLuts, AnLutError> {
    validate_a(a)?;
    Ok(AnLuts {
        and: build_table(a, BinOp::And),
        or: build_table(a, BinOp::Or),
        xor: build_table(a, BinOp::Xor),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(tab: &Table, c1: u32, c2: u32) -> i32 {
        tab[((c1 as usize) << 8) | c2 as usize]
    }

    #[test]
    fn validate_rejects_zero_and_high() {
        assert_eq!(validate_a(0), Err(AnLutError::InvalidA(0)));
        assert_eq!(
            validate_a(A_MAX_EXCLUSIVE),
            Err(AnLutError::InvalidA(A_MAX_EXCLUSIVE))
        );
        assert_eq!(
            validate_a(1u64 << 31),
            Err(AnLutError::InvalidA(1u64 << 31))
        );
        assert_eq!(validate_a(1), Ok(()));
        assert_eq!(validate_a(A_MAX_EXCLUSIVE - 1), Ok(()));
    }

    #[test]
    fn and_entries_match_formula() {
        let a: u64 = 65521;
        let luts = generate(a).unwrap();
        for &(c1, c2) in &[(0u32, 0u32), (0xAB, 0xCD), (0xFF, 0xFF), (0x12, 0x80)] {
            assert_eq!(entry(&luts.and, c1, c2), (a as i32) * ((c1 & c2) as i32));
        }
    }

    #[test]
    fn or_entries_match_formula() {
        let a: u64 = 1009;
        let luts = generate(a).unwrap();
        for &(c1, c2) in &[(0u32, 0u32), (0xAB, 0xCD), (0xFF, 0x00), (0x12, 0x80)] {
            assert_eq!(entry(&luts.or, c1, c2), (a as i32) * ((c1 | c2) as i32));
        }
    }

    #[test]
    fn xor_entries_match_formula() {
        // Largest legal A: (2^23 - 1). Stresses the i32 bound — entry
        // A * 0xFF = 2,139,094,785 still < i32::MAX.
        let a: u64 = A_MAX_EXCLUSIVE - 1;
        let luts = generate(a).unwrap();
        for &(c1, c2) in &[(0u32, 0u32), (0xAB, 0xCD), (0xFF, 0xFF), (0x12, 0x80)] {
            assert_eq!(
                entry(&luts.xor, c1, c2),
                (a as i32).wrapping_mul((c1 ^ c2) as i32)
            );
        }
    }

    #[test]
    fn full_roundtrip_decode() {
        let a: u64 = 65521;
        let luts = generate(a).unwrap();
        for c1 in 0u32..256 {
            for c2 in 0u32..256 {
                assert_eq!(entry(&luts.and, c1, c2) / (a as i32), (c1 & c2) as i32);
                assert_eq!(entry(&luts.or, c1, c2) / (a as i32), (c1 | c2) as i32);
                assert_eq!(entry(&luts.xor, c1, c2) / (a as i32), (c1 ^ c2) as i32);
            }
        }
    }

    #[test]
    fn a_one_is_identity() {
        let luts = generate(1).unwrap();
        for c1 in 0u32..256 {
            for c2 in 0u32..256 {
                assert_eq!(entry(&luts.and, c1, c2), (c1 & c2) as i32);
                assert_eq!(entry(&luts.or, c1, c2), (c1 | c2) as i32);
                assert_eq!(entry(&luts.xor, c1, c2), (c1 ^ c2) as i32);
            }
        }
    }
}
