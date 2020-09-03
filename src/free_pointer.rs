use std::num::NonZeroUsize;

/// Contains a reference to a free slot in an arena, encapsulating NonZeroUsize
/// to prevent off-by-one errors and leaking unsafety.
///
/// Uses NonZeroUsize to stay small when put inside an `Option`.
#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub(crate) struct FreePointer(NonZeroUsize);

impl FreePointer {
    #[must_use]
    pub(crate) fn from_slot(slot: usize) -> Self {
        let value = slot
            .checked_add(1)
            .expect("usize overflowed calculating free pointer from usize");

        // This is safe because any usize + 1 that didn't overflow must not be
        // zero.
        FreePointer(unsafe { NonZeroUsize::new_unchecked(value) })
    }

    #[must_use]
    pub(crate) fn slot(self) -> usize {
        self.0.get() - 1
    }
}

#[cfg(test)]
mod test {
    use super::FreePointer;

    #[test]
    fn from_slot() {
        let ptr = FreePointer::from_slot(0);
        assert_eq!(ptr.slot(), 0);
    }

    #[test]
    #[should_panic(expected = "usize overflowed calculating free pointer from usize")]
    fn panic_on_overflow() {
        let _ = FreePointer::from_slot(std::usize::MAX);
    }
}
