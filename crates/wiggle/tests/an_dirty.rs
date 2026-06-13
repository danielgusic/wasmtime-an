//! Tests for the AN-encoding host-write range recording in `GuestMemory`.
//!
//! When a `GuestMemory` view is created with `unshared_an_tracked`, every
//! mutable access (typed `write`, `copy_from_slice`, `as_slice_mut`) must
//! record the written byte range so the wiggle-generated hostcall wrapper
//! can re-encode the AN-encoding shadow for exactly those ranges.

use wiggle::{GuestMemory, GuestPtr};

#[test]
fn records_typed_writes() {
    let mut buf = vec![0u8; 4096];
    let mut mem = GuestMemory::unshared_an_tracked(&mut buf);
    mem.write(GuestPtr::<u32>::new(64), 0xAABB_CCDDu32).unwrap();
    mem.write(GuestPtr::<u8>::new(100), 7u8).unwrap();
    let dirty = mem.an_take_dirty();
    assert_eq!(dirty, vec![64..68, 100..101]);
    // Draining leaves the list empty.
    assert!(mem.an_take_dirty().is_empty());
}

#[test]
fn float_and_pointer_writes_delegate_to_integer_writes() {
    let mut buf = vec![0u8; 4096];
    let mut mem = GuestMemory::unshared_an_tracked(&mut buf);
    mem.write(GuestPtr::<f64>::new(8), 1.5f64).unwrap();
    mem.write(GuestPtr::<GuestPtr<u8>>::new(32), GuestPtr::<u8>::new(7))
        .unwrap();
    assert_eq!(mem.an_take_dirty(), vec![8..16, 32..36]);
}

#[test]
fn coalesces_adjacent_writes() {
    let mut buf = vec![0u8; 4096];
    let mut mem = GuestMemory::unshared_an_tracked(&mut buf);
    mem.write(GuestPtr::<u32>::new(64), 1u32).unwrap();
    mem.write(GuestPtr::<u32>::new(68), 2u32).unwrap();
    mem.write(GuestPtr::<u32>::new(72), 3u32).unwrap();
    assert_eq!(mem.an_take_dirty(), vec![64..76]);
}

#[test]
fn records_as_slice_mut_and_copy_from_slice() {
    let mut buf = vec![0u8; 4096];
    let mut mem = GuestMemory::unshared_an_tracked(&mut buf);
    let ptr = GuestPtr::<u8>::new(200).as_array(16);
    let _ = mem.as_slice_mut(ptr).unwrap().unwrap();
    let data = [1u8, 2, 3];
    mem.copy_from_slice(&data, GuestPtr::<u8>::new(300).as_array(3))
        .unwrap();
    assert_eq!(mem.an_take_dirty(), vec![200..216, 300..303]);
}

#[test]
fn untracked_view_records_nothing() {
    let mut buf = vec![0u8; 4096];
    let mut mem = GuestMemory::unshared(&mut buf);
    mem.write(GuestPtr::<u32>::new(64), 1u32).unwrap();
    let ptr = GuestPtr::<u8>::new(200).as_array(16);
    let _ = mem.as_slice_mut(ptr).unwrap().unwrap();
    assert!(mem.an_take_dirty().is_empty());
}

#[test]
fn collapses_to_bounding_range_on_overflow() {
    let mut buf = vec![0u8; 4096];
    let mut mem = GuestMemory::unshared_an_tracked(&mut buf);
    // Disjoint single-byte writes (stride 2 prevents coalescing) past the
    // internal range-list cap must collapse into one bounding range rather
    // than grow without bound.
    for i in 0..129u32 {
        mem.write(GuestPtr::<u8>::new(i * 2), 1u8).unwrap();
    }
    let dirty = mem.an_take_dirty();
    assert_eq!(dirty.len(), 1);
    assert_eq!(dirty[0], 0..257);
}

#[test]
fn failed_write_records_nothing() {
    let mut buf = vec![0u8; 64];
    let mut mem = GuestMemory::unshared_an_tracked(&mut buf);
    // Out-of-bounds write fails validation before touching memory.
    assert!(mem.write(GuestPtr::<u32>::new(62), 1u32).is_err());
    assert!(mem.an_take_dirty().is_empty());
}
