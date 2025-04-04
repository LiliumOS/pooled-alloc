use core::{
    alloc::{GlobalAlloc, Layout},
    array,
};
use std::alloc::{Allocator, System};

use crate::pooled_alloc::PooledAlloc;

#[repr(C, align(4096))]
pub struct PageAligned([u8; 4096]);

static ALLOC: PooledAlloc<System> = PooledAlloc::new(System);

const LAYOUTS: [Layout; 6] = [
    Layout::new::<u8>(),
    Layout::new::<[u8; 37]>(),
    Layout::new::<[u64; 15]>(),
    Layout::new::<PageAligned>(),
    Layout::new::<u16>(),
    Layout::new::<[u16; 13]>(),
];

#[test]
fn test_valid_alloc() {
    for layout in LAYOUTS {
        let Ok(alloc) = ALLOC.allocate(layout) else {
            continue;
        };

        assert!(
            layout.size() <= alloc.len(),
            "Layout {layout:?}. Alloc: {alloc:p}",
        );
        assert!(
            alloc.is_aligned_to(layout.align()),
            "Layout {layout:?}. Alloc: {alloc:p} ",
        );

        // SAFETY:
        // We just allocated this above
        unsafe {
            ALLOC.deallocate(alloc.cast(), layout);
        }
    }
}

#[test]
fn test_alloc_zeroed() {
    for layout in LAYOUTS {
        let Ok(alloc) = ALLOC.allocate_zeroed(layout) else {
            continue;
        };

        assert!(
            layout.size() <= alloc.len(),
            "Layout {layout:?}. Alloc: {alloc:p}",
        );
        assert!(
            alloc.is_aligned_to(layout.align()),
            "Layout {layout:?}. Alloc: {alloc:p}",
        );

        for b in unsafe { alloc.as_ref() } {
            assert_eq!(*b, 0);
        }

        // SAFETY:
        // We just allocated this above
        unsafe {
            ALLOC.deallocate(alloc.cast(), layout);
        }
    }
}

#[test]
fn test_box() {
    let b = Box::new_in([0xFFu8; 127], &ALLOC);

    for b in *b {
        assert_eq!(b, 0xFF);
    }
}

#[test]
fn test_repeated_allocs() {
    let mut v: [_; 8] = array::from_fn(|_| Box::new_in([!0usize; 8], &ALLOC));

    let r = v.each_mut();

    let mut r = r.map(|v| &mut **v);

    for (v, m) in r.iter_mut().enumerate() {
        **m = [r; 8];
    }

    for (v, m) in r.iter_mut().enumerate() {
        assert_eq!(**m, [v; 8]);
    }
}
