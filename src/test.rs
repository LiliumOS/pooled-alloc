use core::alloc::{GlobalAlloc, Layout};
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
            "Layout {layout:?}. Alloc: {alloc:p} (len: {})",
            alloc.len()
        );
        assert!(
            alloc.is_aligned_to(layout.align()),
            "Layout {layout:?}. Alloc: {alloc:p} (len: {})",
            alloc.len()
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
            "Layout {layout:?}. Alloc: {alloc:p} (len: {})",
            alloc.len()
        );
        assert!(
            alloc.is_aligned_to(layout.align()),
            "Layout {layout:?}. Alloc: {alloc:p} (len: {})",
            alloc.len()
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
