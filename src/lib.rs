#![cfg_attr(not(test), no_std)]
#![feature(
    allocator_api,
    slice_ptr_get,
    pointer_is_aligned_to,
    sync_unsafe_cell,
    alloc_layout_extra
)]

use core::{num::NonZeroUsize, ops::Div};

#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq)]
pub struct BlockSize(NonZeroUsize);

impl BlockSize {
    /// Constructs a new block size. This is a sized used by the arena in a given pool
    ///
    /// # Safety
    /// The following must be upheld by the caller:
    /// * `val` must be a power of two, and
    /// * `val` must not exceed `isize::MAX >> 12`
    #[inline(always)]
    pub const unsafe fn new_unchecked(val: usize) -> Self {
        unsafe {
            core::hint::assert_unchecked(val.is_power_of_two() && (val << 13) != 0);
        }

        // SAFETY:
        // Assured by Precondition
        Self(unsafe { NonZeroUsize::new_unchecked(val) })
    }

    pub const fn new(val: usize) -> Option<Self> {
        if val.is_power_of_two() && (val << 13) != 0 {
            // Safety: Validated above
            Some(unsafe { Self::new_unchecked(val) })
        } else {
            None
        }
    }

    /// The expected size of a pool with this block_size, including metadata
    #[inline(always)]
    pub const fn expected_pool_size(self) -> usize {
        // SAFETY:
        // The invariant of the type (that )
        let val = unsafe { self.get().unchecked_mul(POOL_SCALE) };

        if val < MIN_POOL_SIZE {
            MIN_POOL_SIZE
        } else {
            val
        }
    }

    #[inline(always)]
    pub const fn get(self) -> usize {
        let val = self.0.get();
        unsafe {
            core::hint::assert_unchecked(val.is_power_of_two());
        }
        val
    }

    #[inline(always)]
    pub const fn ilog2(self) -> u32 {
        self.0.trailing_zeros()
    }

    #[inline(always)]
    pub const fn ceil_to(self, val: usize) -> usize {
        let r = self.get() - 1;
        (val + r) & !r
    }

    #[inline(always)]
    pub const fn div_ceil_to(self, val: usize) -> usize {
        (val + (self.get() - 1)) >> self.ilog2()
    }
}

impl Div<BlockSize> for usize {
    type Output = Self;
    #[inline(always)]
    fn div(self, rhs: BlockSize) -> Self::Output {
        self >> rhs.ilog2()
    }
}

pub mod pool;
pub mod pooled_alloc;

pub const SMALLEST_POOL: usize = usize::BITS as usize;

/// The smallest total size (in bytes) a single pool will take on.
///
/// ## Note
/// This is guaranteed to be a power of two that is at least 4096.
///
/// ## Implementation Note
/// The current value is the square of the bit size of a pointer, times 8, but this is not a stable guarantee.
pub const MIN_POOL_SIZE: usize = SMALLEST_POOL * SMALLEST_POOL * 8;

/// The minimum number of elements that will be allocated at a time.
///
/// ## Note
/// This value is guaranteed to never exceed 4096.
///
/// ## Implementation Note
/// The current value is four times the bit size of a pointer, but this is not a stable guarantee.
pub const POOL_SCALE: usize = (usize::BITS as usize) * 4;

#[cfg(test)]
mod test;
