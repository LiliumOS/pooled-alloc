use core::{
    alloc::{Allocator, GlobalAlloc, Layout},
    num::NonZeroUsize,
    ops::Div,
    ptr::NonNull,
    sync::atomic::{AtomicPtr, AtomicUsize, Ordering},
};

use crate::BlockSize;

#[cfg_attr(target_pointer_width = "64", repr(align(8)))]
#[cfg_attr(target_pointer_width = "32", repr(align(4)))]
pub struct Pool<A: Allocator> {
    base: NonNull<u8>,
    total_size: usize,

    next_ptr: AtomicPtr<u8>,
    block_len_bits: u32,
    page_alloc: A,
}

unsafe impl<A: Send + Allocator> Send for Pool<A> {}
unsafe impl<A: Sync + Allocator> Sync for Pool<A> {}

impl<A: Allocator> Drop for Pool<A> {
    fn drop(&mut self) {
        let layout =
            Self::layout_for_pool(unsafe { BlockSize::new_unchecked(1 << self.block_len_bits) });

        if let Some(next) = self.next_pool_ptr() {
            unsafe {
                core::ptr::drop_in_place(next.as_ptr());
            }
        }

        unsafe {
            self.page_alloc.deallocate(self.base, layout);
        }
    }
}

impl<A: Allocator> Pool<A> {
    fn layout_for_pool(block_len: BlockSize) -> Layout {
        let asize = block_len.expected_pool_size();
        let align = 4096;

        unsafe { Layout::from_size_align_unchecked(asize, align) }
    }

    fn next_pool_ptr(&self) -> Option<NonNull<Self>> {
        let pool = self.base.cast::<Self>();

        if unsafe {
            (&raw const (*pool.as_ptr()).base)
                .cast::<Option<NonNull<u8>>>()
                .read()
        }
        .is_none()
        {
            None
        } else {
            Some(pool)
        }
    }

    fn allocate_next(&self) -> Result<&Self, core::alloc::AllocError>
    where
        A: Clone,
    {
        let pool = self.base.cast::<Self>();
        if unsafe {
            (&raw const (*pool.as_ptr()).base)
                .cast::<Option<NonNull<u8>>>()
                .read()
        }
        .is_none()
        {
            unsafe {
                pool.write(Self::allocate_pool(
                    BlockSize::new_unchecked(1 << self.block_len_bits),
                    self.page_alloc.clone(),
                )?);
            }
        }

        Ok(unsafe { pool.as_ref() })
    }

    fn arena(&self) -> &[AtomicUsize] {
        let base = unsafe { self.base.cast::<Self>().add(1).cast::<AtomicUsize>() };
        let len = (self.total_size / (1 << self.block_len_bits)) >> 3;

        unsafe { core::slice::from_raw_parts(base.as_ptr(), len) }
    }

    pub fn next_pool(&self) -> Option<&Self> {
        self.next_pool_ptr().map(unsafe { |v| v.as_ref() })
    }

    /// Counts the number of allocated blocks in `self` (including it's children)
    /// This is in multiples of [`Self::block_size`]
    pub fn count_allocated(&self) -> usize {
        let mut val = 0;

        for step in self.arena() {
            val += step.load(Ordering::Relaxed).count_ones() as usize;
        }

        val + self.next_pool().map_or(0, |r| r.count_allocated())
    }

    pub const fn block_len(&self) -> BlockSize {
        unsafe { BlockSize::new_unchecked(1 << self.block_len_bits) }
    }

    pub fn allocate_pool(block_len: BlockSize, alloc: A) -> Result<Self, core::alloc::AllocError> {
        const { assert!(core::mem::size_of::<A>() <= 2 * core::mem::size_of::<usize>()) }
        let block_len_bits = block_len.ilog2();
        let layout = Self::layout_for_pool(block_len);
        let asize = layout.size();

        let bsize = asize / block_len.get();
        let arena_size = bsize / 8;

        let aptr = alloc.allocate_zeroed(layout)?.as_non_null_ptr();

        let aligned_start = block_len.ceil_to((arena_size + core::mem::size_of::<Self>()));

        let alloc_ptr = unsafe { aptr.add(aligned_start) };

        let mut this = Self {
            block_len_bits,
            base: aptr,
            total_size: asize,
            next_ptr: AtomicPtr::new(alloc_ptr.as_ptr()),
            page_alloc: alloc,
        };

        let len = aligned_start >> block_len_bits;

        let mask = !(!0 << (len as u32));

        unsafe {
            alloc_ptr.cast::<usize>().write(mask);
        }

        Ok(this)
    }

    fn find_alloc_for_impl(&self, ptr: *mut u8) -> Option<&Self> {
        let base = self.base.as_ptr();
        let end = unsafe { base.add(self.total_size) };

        if (base..end).contains(&ptr) {
            return Some(self);
        } else {
            self.next_pool().and_then(|pool| pool.find_alloc_for(ptr))
        }
    }

    pub fn find_alloc_for(&self, ptr: *mut u8) -> Option<&Self> {
        if !ptr.is_aligned_to(1 << self.block_len_bits) {
            return None; // Short-Circuit as all nested pools have no 
        }
        self.find_alloc_for_impl(ptr)
    }

    fn try_allocate_slow(&self, count: usize) -> *mut u8 {
        todo!()
    }

    unsafe fn mark_deallocated_slow(&self, ptr: *mut u8, count: usize) {
        todo!()
    }

    pub unsafe fn mark_deallocated(&self, ptr: *mut u8, count: usize) {
        if count > (usize::BITS as usize) {
            unsafe {
                self.mark_deallocated_slow(ptr, count);
            }
        } else {
            let slot =
                unsafe { ptr.offset_from_unsigned(self.base.as_ptr()) >> (self.block_len_bits) };

            let offset = (slot / usize::BITS as usize);
            let start_pos = (slot as u32 & (usize::BITS - 1));
            let mask = (!(!0 << count)) << start_pos;

            let slab = &self.arena()[offset];

            slab.fetch_nand(mask, Ordering::Release);
        }
    }

    pub fn try_allocate(&self, count: usize) -> *mut u8
    where
        A: Clone,
    {
        if count.saturating_mul(1 << self.block_len_bits) > self.total_size {
            return core::ptr::null_mut();
        }

        if count > (usize::BITS as usize) {
            self.try_allocate_slow(count)
        } else {
            let count = count as u32;
            let start = self.next_ptr.load(Ordering::Acquire);

            let slot =
                unsafe { start.offset_from_unsigned(self.base.as_ptr()) >> (self.block_len_bits) };

            let mut offset = (slot / usize::BITS as usize);
            let mut start_pos = (slot as u32 & (usize::BITS - 1));

            let mut pass_two = false;

            let arena = self.arena();

            loop {
                if (start_pos + count) > usize::BITS {
                    offset += 1;
                    start_pos = 0;
                }
                if offset > arena.len() {
                    if pass_two {
                        break self
                            .allocate_next()
                            .map_or(core::ptr::null_mut(), |f| f.try_allocate(count as usize));
                    } else {
                        offset = 0;
                        start_pos = 0;
                        pass_two = true;
                        continue;
                    }
                }

                let slab = arena[offset].load(Ordering::Acquire);

                if (slab >> start_pos).trailing_zeros() >= count {
                    let mask = (!(!0 << count)) << start_pos;

                    match arena[offset].compare_exchange(
                        slab,
                        slab | mask,
                        Ordering::Acquire,
                        Ordering::Relaxed,
                    ) {
                        Ok(v) => {
                            let offset = ((offset * usize::BITS as usize) | start_pos as usize)
                                << self.block_len_bits;
                            break unsafe { self.base.add(offset).as_ptr() };
                        }
                        Err(_) => continue,
                    }
                } else {
                    start_pos += (slab >> start_pos).trailing_zeros();
                    start_pos += (slab >> start_pos).trailing_ones();
                }
            }
        }
    }
}
