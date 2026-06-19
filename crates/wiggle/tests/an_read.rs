//! Tests for AN-encoding host-*read* verification in `GuestMemory`.
//!
//! A view built with `unshared_an_verified` carries the encoded shadow, so
//! every host read (`read`, `as_slice`/`as_cow`/`as_str`, `to_vec`) must
//! cross-check exactly the byte range it touches against that shadow and
//! surface a raw/shadow divergence as `GuestError::AnMemoryMismatch` — without
//! ever scanning the whole memory.

use wiggle::{GuestError, GuestMemory, GuestPtr};

const A: u64 = 65521;

/// Encode `raw` into a fresh shadow: each 4-byte little-endian raw slot maps to
/// an 8-byte little-endian `A * u32_le(slot)` shadow slot. `raw.len()` is a
/// multiple of 4 in these tests.
fn encode(raw: &[u8], a: u64) -> Vec<u8> {
    let mut shadow = vec![0u8; raw.len() * 2];
    for (i, chunk) in raw.chunks_exact(4).enumerate() {
        let word = u32::from_le_bytes(chunk.try_into().unwrap());
        let enc = a.wrapping_mul(u64::from(word));
        shadow[i * 8..i * 8 + 8].copy_from_slice(&enc.to_le_bytes());
    }
    shadow
}

/// A `read` of an untampered slot passes.
#[test]
fn clean_typed_read_passes() {
    let mut raw = vec![0u8; 64];
    raw[16..20].copy_from_slice(&7u32.to_le_bytes());
    let shadow = encode(&raw, A);
    let mem = GuestMemory::unshared_an_verified(&mut raw, &shadow, A);
    assert_eq!(mem.read(GuestPtr::<u32>::new(16)).unwrap(), 7);
}

/// A `read` over a raw slot that no longer matches the shadow must surface
/// `AnMemoryMismatch` rather than hand the host the corrupted value.
#[test]
fn tampered_typed_read_traps() {
    let mut raw = vec![0u8; 64];
    raw[16..20].copy_from_slice(&7u32.to_le_bytes());
    let shadow = encode(&raw, A);
    // Diverge raw from the shadow *after* encoding (simulates corruption).
    raw[16] = 0xFF;
    let mem = GuestMemory::unshared_an_verified(&mut raw, &shadow, A);
    assert_eq!(
        mem.read(GuestPtr::<u32>::new(16)),
        Err(GuestError::AnMemoryMismatch),
    );
}

/// Same divergence, observed through `as_slice` (covers `as_cow`/`as_str`).
#[test]
fn tampered_slice_read_traps() {
    let mut raw = vec![0u8; 64];
    raw[32..36].copy_from_slice(&0xABCD_1234u32.to_le_bytes());
    let shadow = encode(&raw, A);
    raw[34] = 0x00;
    let mem = GuestMemory::unshared_an_verified(&mut raw, &shadow, A);
    assert_eq!(
        mem.as_slice(GuestPtr::<u8>::new(32).as_array(4)).err(),
        Some(GuestError::AnMemoryMismatch),
    );
}

/// Same divergence, observed through `to_vec`.
#[test]
fn tampered_to_vec_read_traps() {
    let mut raw = vec![0u8; 64];
    raw[40..44].copy_from_slice(&0x1111_2222u32.to_le_bytes());
    let shadow = encode(&raw, A);
    raw[41] = 0x99;
    let mem = GuestMemory::unshared_an_verified(&mut raw, &shadow, A);
    assert_eq!(
        mem.to_vec(GuestPtr::<u8>::new(40).as_array(4)).err(),
        Some(GuestError::AnMemoryMismatch),
    );
}

/// A read of a slot the host itself wrote this call is skipped: the write is
/// recorded in `an_dirty` and the shadow is re-encoded only after the call, so
/// the captured shadow is legitimately stale there and must not false-trap.
#[test]
fn host_written_slot_is_skipped() {
    let mut raw = vec![0u8; 64];
    raw[16..20].copy_from_slice(&7u32.to_le_bytes());
    let shadow = encode(&raw, A);
    let mut mem = GuestMemory::unshared_an_verified(&mut raw, &shadow, A);
    // Host writes the slot (diverging raw from the captured shadow) and records
    // it dirty; reading it back must not trap.
    mem.write(GuestPtr::<u32>::new(16), 0xDEAD_BEEFu32).unwrap();
    assert_eq!(mem.read(GuestPtr::<u32>::new(16)).unwrap(), 0xDEAD_BEEF);
}

/// A view without a shadow (`unshared_an_tracked`) does not verify reads, so a
/// raw/shadow divergence is invisible — confirms read-verify is gated on the
/// shadow being handed to the view.
#[test]
fn tracked_only_view_does_not_verify() {
    let mut raw = vec![0u8; 64];
    raw[16..20].copy_from_slice(&7u32.to_le_bytes());
    let _shadow = encode(&raw, A);
    raw[16] = 0xFF;
    let mem = GuestMemory::unshared_an_tracked(&mut raw);
    // raw[16..20] was 7u32 LE = [0x07,0,0,0]; after raw[16]=0xFF it reads 0xFF.
    assert_eq!(mem.read(GuestPtr::<u32>::new(16)).unwrap(), 0xFF);
}
