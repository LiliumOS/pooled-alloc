use core::alloc::AllocError;
use core::{
    alloc::{Allocator, GlobalAlloc, Layout},
    cell::SyncUnsafeCell,
    hint::spin_loop,
    mem::MaybeUninit,
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
};

use crate::{BlockSize, POOL_SCALE, SMALLEST_POOL, pool::Pool};

#[cfg(target_pointer_width = "32")]
pub const DEFAULT_POOL_SIZES: [BlockSize; 4] = unsafe {
    [
        BlockSize::new_unchecked(32),
        BlockSize::new_unchecked(128),
        BlockSize::new_unchecked(512),
        BlockSize::new_unchecked(2048),
    ]
};

#[cfg(target_pointer_width = "64")]
pub const DEFAULT_POOL_SIZES: [BlockSize; 3] = unsafe {
    [
        BlockSize::new_unchecked(64),
        BlockSize::new_unchecked(256),
        BlockSize::new_unchecked(1024),
    ]
};

/// An Allocator that allocates using a series of pool.
///
/// `A` is an underlying allocator used to fetch backing memory. This is expected to yield multiple-page sized, page-aligned allocations, but not smaller.
/// This generally should be backed by a large static array (at least 4096 pages) or by calls to the OS page allocator (`mmap`, `VirtualAlloc`).
/// A need not be efficient for small allocations (either in time or in size).
/// Note that the [`PooledAlloc`] implementation expects that [`Allocator::allocate_zeroed`] (for `A`) is equally efficient to [`Allocator::allocate`]
pub struct PooledAlloc<A: Allocator> {
    alloc: A,
    pool_init_tracker: AtomicUsize,
    pools: [SyncUnsafeCell<MaybeUninit<Pool<A>>>; DEFAULT_POOL_SIZES.len()],
}

impl<A: Allocator> Drop for PooledAlloc<A> {
    fn drop(&mut self) {
        let v = *self.pool_init_tracker.get_mut();
        for i in 0..DEFAULT_POOL_SIZES.len() {
            if (v & (1 << i)) == 1 {
                unsafe {
                    MaybeUninit::assume_init_drop(self.pools[i].get_mut());
                }
            }
        }
    }
}

impl<A: Allocator> PooledAlloc<A> {
    pub const fn new(alloc: A) -> Self {
        Self {
            alloc,
            pool_init_tracker: AtomicUsize::new(0),
            pools: [const { SyncUnsafeCell::new(MaybeUninit::uninit()) }; DEFAULT_POOL_SIZES.len()],
        }
    }

    fn pool(&self, pool: usize) -> Option<&Pool<A>> {
        let ptr = self.pools[pool].get().cast::<Pool<A>>();
        let init = 1 << pool;
        if (self.pool_init_tracker.load(Ordering::Acquire) & init) == 1 {
            Some(unsafe { &*ptr })
        } else {
            None
        }
    }

    fn find_pool_for_alloc(&self, ptr: *mut u8, size: usize, align: usize) -> Option<&Pool<A>> {
        let min_pool = match align {
            0..SMALLEST_POOL => 0,
            4096.. => DEFAULT_POOL_SIZES.len(),
            x => ((x.trailing_zeros() - SMALLEST_POOL.trailing_zeros()) as usize + 1) >> 1,
        };

        let expected_pool = match size {
            0..SMALLEST_POOL => 0,
            4096.. => DEFAULT_POOL_SIZES.len(),
            x => (x.ilog2() as usize - SMALLEST_POOL) >> 1,
        };

        let expected_pool = expected_pool.max(min_pool); // Don't expect an impossible pool.

        if min_pool == DEFAULT_POOL_SIZES.len() {
            return None; // No possible pool.
        }

        let mut check_pool = expected_pool;

        loop {
            if let Some(pool) = self.pool(check_pool) {
                if let Some(pool) = pool.find_alloc_for(ptr) {
                    break Some(pool);
                }
            }

            if check_pool == min_pool {
                check_pool = DEFAULT_POOL_SIZES.len() - 1;
            } else {
                check_pool -= 1;
            }

            if check_pool == expected_pool {
                break None;
            }
        }
    }
}

struct UnlockOnDrop<'a>(&'a AtomicUsize, usize);

impl<'a> Drop for UnlockOnDrop<'a> {
    fn drop(&mut self) {
        self.0.fetch_nand(self.1, Ordering::Relaxed);
    }
}

impl<A: Allocator + Clone> PooledAlloc<A> {
    fn init_pool(&self, pool: usize) -> Result<&Pool<A>, AllocError> {
        let init = 1 << pool;
        let locked = 0x100 << pool;
        let ptr = self.pools[pool].get().cast::<Pool<A>>();
        if (self.pool_init_tracker.load(Ordering::Acquire) & init) == 1 {
            return Ok(unsafe { &*ptr });
        }

        let mut v;

        while {
            v = self.pool_init_tracker.fetch_or(locked, Ordering::Acquire);
            v
        } & locked
            != 0
        {
            spin_loop();
        }

        if (v & init) != 0 {
            self.pool_init_tracker.fetch_nand(locked, Ordering::Release);
            return Ok(unsafe { &*ptr });
        }

        let guard = UnlockOnDrop(&self.pool_init_tracker, locked);

        let p = Pool::allocate_pool(DEFAULT_POOL_SIZES[pool], self.alloc.clone())?;

        core::mem::forget(guard);

        unsafe {
            ptr.write(p);
        }

        // Note:
        // This both unsets the locked bit and sets the init bit in a single operation.
        // This is valid because we know that init is unset and locked is set at this point (due to the above lock and init-check)
        // This isn't a store because the other init bits and lock bits can be in arbitrary states.
        self.pool_init_tracker
            .fetch_xor(init | locked, Ordering::Release);

        Ok(unsafe { &*ptr })
    }
}

unsafe impl<A: Allocator + Clone> Allocator for PooledAlloc<A> {
    fn allocate(
        &self,
        layout: core::alloc::Layout,
    ) -> Result<core::ptr::NonNull<[u8]>, AllocError> {
        if layout.size() == 0 {
            return Ok(NonNull::slice_from_raw_parts(layout.dangling(), 0));
        }

        if layout.align() > 4096 {
            return Err(AllocError);
        }

        let layout = layout.pad_to_align();

        let mut size_class = match layout.size() {
            0..=SMALLEST_POOL => 0,
            4096.. => DEFAULT_POOL_SIZES.len(),
            x => (x.div_ceil(SMALLEST_POOL).ilog2()) as usize >> 1,
        };

        let blk_size = (1 << (size_class << 1)) * SMALLEST_POOL;

        if blk_size < layout.align() {
            size_class = size_class + 1;
        }

        if let Some(&blk_sz) = DEFAULT_POOL_SIZES.get(size_class) {
            let pool = self.init_pool(size_class)?;
            let total_size = blk_sz.ceil_to(layout.size());
            let count = total_size / blk_sz;
            let ptr = pool.try_allocate(count);

            let ptr = NonNull::new(ptr).ok_or(AllocError)?;

            Ok(NonNull::slice_from_raw_parts(ptr, total_size))
        } else {
            self.alloc.allocate_zeroed(layout)
        }
    }

    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        if layout.size() == 0 {
            return Ok(NonNull::slice_from_raw_parts(layout.dangling(), 0));
        }

        if layout.align() > 4096 {
            return Err(AllocError);
        }

        let layout = layout.pad_to_align();

        let mut size_class = match layout.size() {
            0..=SMALLEST_POOL => 0,
            4096.. => DEFAULT_POOL_SIZES.len(),
            x => (x.div_ceil(SMALLEST_POOL).ilog2()) as usize >> 1,
        };

        let blk_size = (1 << (size_class << 1)) * SMALLEST_POOL;

        if blk_size < layout.align() {
            size_class = size_class + 1;
        }

        if let Some(&blk_sz) = DEFAULT_POOL_SIZES.get(size_class) {
            let pool = self.init_pool(size_class)?;
            let total_size = blk_sz.ceil_to(layout.size());
            let count = total_size / blk_sz;
            let ptr = pool.try_allocate(count);

            let ptr = NonNull::new(ptr).ok_or(AllocError)?;

            unsafe {
                core::ptr::write_bytes(ptr.as_ptr(), 0, total_size);
            }

            let ret = NonNull::slice_from_raw_parts(ptr, total_size);

            Ok(ret)
        } else {
            self.alloc.allocate_zeroed(layout)
        }
    }

    unsafe fn deallocate(&self, ptr: core::ptr::NonNull<u8>, layout: core::alloc::Layout) {
        if layout.size() == 0 {
            return;
        }

        if let Some(pool) = self.find_pool_for_alloc(ptr.as_ptr(), layout.size(), layout.align()) {
            let chunks = pool.block_len().div_ceil_to(layout.size());
            unsafe {
                pool.mark_deallocated(ptr.as_ptr(), chunks);
            }
        } else {
            unsafe { self.alloc.deallocate(ptr, layout) }
        }
    }
}

unsafe impl<A: Allocator + Clone> GlobalAlloc for PooledAlloc<A> {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        self.allocate(layout)
            .map_or_else(|_| core::ptr::null_mut(), |v| v.as_ptr().cast())
    }

    unsafe fn alloc_zeroed(&self, layout: core::alloc::Layout) -> *mut u8 {
        self.allocate_zeroed(layout)
            .map_or_else(|_| core::ptr::null_mut(), |v| v.as_ptr().cast())
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: core::alloc::Layout) {
        unsafe {
            self.deallocate(NonNull::new_unchecked(ptr), layout);
        }
    }

    unsafe fn realloc(
        &self,
        ptr: *mut u8,
        layout: core::alloc::Layout,
        new_size: usize,
    ) -> *mut u8 {
        let ptr = unsafe { NonNull::new_unchecked(ptr) };
        let new_layout = unsafe { Layout::from_size_align_unchecked(new_size, layout.align()) };
        let res = if new_size < layout.size() {
            unsafe { self.shrink(ptr, layout, new_layout) }
        } else {
            unsafe { self.grow(ptr, layout, new_layout) }
        };

        res.map_or_else(|_| core::ptr::null_mut(), |v| v.as_ptr().cast())
    }
}
