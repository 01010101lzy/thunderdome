#![deny(clippy::integer_arithmetic)]

use std::convert::TryInto;
use std::iter::{ExactSizeIterator, FusedIterator};
use std::mem::replace;
use std::ops;

use crate::free_pointer::FreePointer;
use crate::generation::Generation;

/// Container that can have elements inserted into it and removed from it.
///
/// Indices use the [`Index`][Index] type, created by inserting values with
/// [`Arena::insert`][Arena::insert].
#[derive(Debug, Clone)]
pub struct Arena<T> {
    storage: Vec<Entry<T>>,
    len: usize,
    first_free: Option<FreePointer>,
}

/// Index type for [`Arena`][Arena] that has a generation attached to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Index {
    slot: u32,
    generation: Generation,
}

#[derive(Debug, Clone)]
enum Entry<T> {
    Occupied(OccupiedEntry<T>),
    Empty(EmptyEntry),
}

impl<T> Entry<T> {
    /// Consume the entry, and if it's occupied, return the value.
    fn into_value(self) -> Option<T> {
        match self {
            Entry::Occupied(occupied) => Some(occupied.value),
            Entry::Empty(_) => None,
        }
    }

    /// If the entry is empty, return a copy of the emptiness data.
    fn get_empty(&self) -> Option<EmptyEntry> {
        match self {
            Entry::Empty(empty) => Some(*empty),
            Entry::Occupied(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
struct OccupiedEntry<T> {
    generation: Generation,
    value: T,
}

#[derive(Debug, Clone, Copy)]
struct EmptyEntry {
    generation: Generation,
    next_free: Option<FreePointer>,
}

impl<T> Arena<T> {
    /// Construct an empty arena.
    pub fn new() -> Self {
        Self {
            storage: Vec::new(),
            len: 0,
            first_free: None,
        }
    }

    /// Construct an empty arena with space to hold exactly `capacity` elements
    /// without reallocating.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            storage: Vec::with_capacity(capacity),
            len: 0,
            first_free: None,
        }
    }

    /// Return the number of elements contained in the arena.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Return the number of elements the arena can hold without allocating,
    /// including the elements currently in the arena.
    pub fn capacity(&self) -> usize {
        self.storage.capacity()
    }

    /// Returns whether the arena is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Insert a new value into the arena, returning an index that can be used
    /// to later retrieve the value.
    pub fn insert(&mut self, value: T) -> Index {
        // This value will definitely be inserted, so we can update length now.
        self.len = self
            .len
            .checked_add(1)
            .unwrap_or_else(|| panic!("Cannot insert more than u32::MAX elements into Arena"));

        // If there was a previously free entry, we can re-use its slot as long
        // as we increment its generation.
        if let Some(free_pointer) = self.first_free {
            let slot = free_pointer.slot();
            let entry = self.storage.get_mut(slot as usize).unwrap_or_else(|| {
                unreachable!("first_free pointed past the end of the arena's storage")
            });

            let empty = entry
                .get_empty()
                .unwrap_or_else(|| unreachable!("first_free pointed to an occupied entry"));

            // If there is another empty entry after this one, we'll update the
            // arena to point to it to use it on the next insertion.
            self.first_free = empty.next_free;

            // Overwrite the entry directly using our mutable reference instead
            // of indexing into our storage again. This should avoid an
            // additional bounds check.
            let generation = empty.generation.next();
            *entry = Entry::Occupied(OccupiedEntry { generation, value });

            Index { slot, generation }
        } else {
            // There were no more empty entries left in our free list, so we'll
            // create a new first-generation entry and push it into storage.

            let generation = Generation::first();
            let slot: u32 = self.storage.len().try_into().unwrap_or_else(|_| {
                unreachable!("Arena storage exceeded what can be represented by a u32")
            });

            self.storage
                .push(Entry::Occupied(OccupiedEntry { generation, value }));

            Index { slot, generation }
        }
    }

    /// Get an immutable reference to a value inside the arena by
    /// [`Index`][Index], returning `None` if the index is not contained in the
    /// arena.
    pub fn get(&self, index: Index) -> Option<&T> {
        match self.storage.get(index.slot as usize) {
            Some(Entry::Occupied(occupied)) if occupied.generation == index.generation => {
                Some(&occupied.value)
            }
            _ => None,
        }
    }

    /// Get a mutable reference to a value inside the arena by [`Index`][Index],
    /// returning `None` if the index is not contained in the arena.
    pub fn get_mut(&mut self, index: Index) -> Option<&mut T> {
        match self.storage.get_mut(index.slot as usize) {
            Some(Entry::Occupied(occupied)) if occupied.generation == index.generation => {
                Some(&mut occupied.value)
            }
            _ => None,
        }
    }

    /// Remove the value contained at the given index from the arena, returning
    /// it if it was present.
    pub fn remove(&mut self, index: Index) -> Option<T> {
        let entry = self.storage.get_mut(index.slot as usize)?;

        match entry {
            Entry::Occupied(occupied) if occupied.generation == index.generation => {
                // We can replace an occupied entry with an empty entry with the
                // same generation. On next insertion, this generation will
                // increment.
                let new_entry = Entry::Empty(EmptyEntry {
                    generation: occupied.generation,
                    next_free: self.first_free,
                });

                // Swap our new entry into our storage and take ownership of the
                // old entry. We'll consume it for its value so we can give that
                // back to our caller.
                let old_entry = replace(entry, new_entry);
                let value = old_entry.into_value().unwrap_or_else(|| unreachable!());

                // The next time we insert, we can re-use the empty entry we
                // just created. If another removal happens before then, that
                // entry will be used before this one (FILO).
                self.first_free = Some(FreePointer::from_slot(index.slot));

                self.len = self.len.checked_sub(1).unwrap_or_else(|| unreachable!());

                Some(value)
            }
            _ => None,
        }
    }

    /// Returns an iterator that removes each element from the arena.
    ///
    /// Iteration order is not defined.
    ///
    /// If the iterator is dropped before it is fully consumed, any uniterated
    /// items will still be contained in the arena.
    pub fn drain(&mut self) -> Drain<'_, T> {
        Drain {
            arena: self,
            slot: 0,
        }
    }

    /// This method is a lot like `remove`, but takes no generation. It's used
    /// as part of `drain` and can likely be exposed as a public API eventually.
    fn remove_entry_by_slot(&mut self, slot: u32) -> Option<(Index, T)> {
        let entry = self.storage.get_mut(slot as usize)?;

        match entry {
            Entry::Occupied(occupied) => {
                // Construct the index that would be used to access this entry.
                let index = Index {
                    generation: occupied.generation,
                    slot,
                };

                // This occupied entry will be replaced with an empty one of the
                // same generation. Generation will be incremented on the next
                // insert.
                let next_entry = Entry::Empty(EmptyEntry {
                    generation: occupied.generation,
                    next_free: self.first_free,
                });

                // Swap new entry into place and consume the old one.
                let old_entry = replace(entry, next_entry);
                let value = old_entry.into_value().unwrap_or_else(|| unreachable!());

                // Set this entry as the next one that should be inserted into,
                // should an insertion happen.
                self.first_free = Some(FreePointer::from_slot(slot));

                self.len = self.len.checked_sub(1).unwrap_or_else(|| unreachable!());

                Some((index, value))
            }
            _ => None,
        }
    }
}

impl<T> Default for Arena<T> {
    fn default() -> Self {
        Arena::new()
    }
}

impl<T> ops::Index<Index> for Arena<T> {
    type Output = T;

    fn index(&self, index: Index) -> &Self::Output {
        self.get(index)
            .unwrap_or_else(|| panic!("No entry at index {:?}", index))
    }
}

impl<T> ops::IndexMut<Index> for Arena<T> {
    fn index_mut(&mut self, index: Index) -> &mut Self::Output {
        self.get_mut(index)
            .unwrap_or_else(|| panic!("No entry at index {:?}", index))
    }
}

/// See [`Arena::drain`][Arena::drain].
pub struct Drain<'a, T> {
    arena: &'a mut Arena<T>,
    slot: u32,
}

impl<'a, T> Iterator for Drain<'a, T> {
    type Item = (Index, T);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            // If there are no entries remaining in the arena, we should always
            // return None. Using this check instead of comparing with the
            // arena's size allows us to skip any trailing empty entries.
            if self.arena.is_empty() {
                return None;
            }

            let slot = self.slot;

            // In the event that we overflow a u32, we should always panic. Rust
            // will, by default, panic on overflow in debug, but silently wrap
            // in release.
            self.slot = self
                .slot
                .checked_add(1)
                .unwrap_or_else(|| panic!("Overflowed u32 trying to drain Arena"));

            // If this entry is occupied, this method will mark it as an empty.
            // Otherwise, we'll continue looping until we've drained all
            // occupied entries from the arena.
            if let Some((index, value)) = self.arena.remove_entry_by_slot(slot) {
                return Some((index, value));
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.arena.len(), Some(self.arena.len()))
    }
}

impl<'a, T> FusedIterator for Drain<'a, T> {}
impl<'a, T> ExactSizeIterator for Drain<'a, T> {}

#[cfg(test)]
mod test {
    use super::{Arena, Index};

    use std::collections::HashSet;
    use std::mem::size_of;

    #[test]
    fn size_of_index() {
        assert_eq!(size_of::<Index>(), 8);
        assert_eq!(size_of::<Option<Index>>(), 8);
    }

    #[test]
    fn new() {
        let arena: Arena<u32> = Arena::new();
        assert_eq!(arena.len(), 0);
        assert_eq!(arena.capacity(), 0);
    }

    #[test]
    fn with_capacity() {
        let arena: Arena<u32> = Arena::with_capacity(8);
        assert_eq!(arena.len(), 0);
        assert_eq!(arena.capacity(), 8);
    }

    #[test]
    fn insert_and_get() {
        let mut arena = Arena::new();

        let one = arena.insert(1);
        assert_eq!(arena.len(), 1);
        assert_eq!(arena.get(one), Some(&1));

        let two = arena.insert(2);
        assert_eq!(arena.len(), 2);
        assert_eq!(arena.get(one), Some(&1));
        assert_eq!(arena.get(two), Some(&2));
    }

    #[test]
    fn insert_remove_get() {
        let mut arena = Arena::new();
        let one = arena.insert(1);

        let two = arena.insert(2);
        assert_eq!(arena.len(), 2);
        assert_eq!(arena.remove(two), Some(2));

        let three = arena.insert(3);
        assert_eq!(arena.len(), 2);
        assert_eq!(arena.get(one), Some(&1));
        assert_eq!(arena.get(three), Some(&3));
        assert_eq!(arena.get(two), None);
    }

    #[test]
    fn get_mut() {
        let mut arena = Arena::new();
        let foo = arena.insert(5);

        let handle = arena.get_mut(foo).unwrap();
        *handle = 6;

        assert_eq!(arena.get(foo), Some(&6));
    }

    #[test]
    fn insert_remove_insert_capacity() {
        let mut arena = Arena::with_capacity(2);
        assert_eq!(arena.capacity(), 2);

        let a = arena.insert("a");
        let b = arena.insert("b");
        assert_eq!(arena.len(), 2);
        assert_eq!(arena.capacity(), 2);

        arena.remove(a);
        arena.remove(b);
        assert_eq!(arena.len(), 0);
        assert_eq!(arena.capacity(), 2);

        let _a2 = arena.insert("a2");
        let _b2 = arena.insert("b2");
        assert_eq!(arena.len(), 2);
        assert_eq!(arena.capacity(), 2);
    }

    #[test]
    fn drain() {
        let mut arena = Arena::with_capacity(2);
        let one = arena.insert(1);
        let two = arena.insert(2);

        let mut drained_pairs = HashSet::new();
        for (index, value) in arena.drain() {
            assert!(drained_pairs.insert((index, value)));
        }

        assert_eq!(arena.len(), 0);
        assert_eq!(arena.capacity(), 2);
        assert_eq!(drained_pairs.len(), 2);
        assert!(drained_pairs.contains(&(one, 1)));
        assert!(drained_pairs.contains(&(two, 2)));

        // We should still be able to use the arena after this.
        let one_prime = arena.insert(1);
        let two_prime = arena.insert(2);

        assert_eq!(arena.len(), 2);
        assert_eq!(arena.capacity(), 2);
        assert_eq!(arena.get(one_prime), Some(&1));
        assert_eq!(arena.get(two_prime), Some(&2));
        assert_eq!(arena.get(one), None);
        assert_eq!(arena.get(two), None);
    }
}
