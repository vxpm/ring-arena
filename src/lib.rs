use flume::{Receiver, Sender};
use std::{collections::VecDeque, mem::MaybeUninit, ptr::NonNull};

struct ArenaAllocation {
    start: usize,
    len: usize,
}

impl ArenaAllocation {
    #[inline(always)]
    pub fn end(&self) -> usize {
        self.start + self.len
    }
}

enum HandleInner<T> {
    Arena {
        ptr: NonNull<[MaybeUninit<T>]>,
        sender: Sender<NonNull<[MaybeUninit<T>]>>,
    },
    Boxed(Box<[MaybeUninit<T>]>),
}

/// Handle to a `[T]` in a [`RingArena<T>`].
pub struct Handle<T>(HandleInner<T>);

unsafe impl<T> Send for Handle<T> where T: Sync {}
unsafe impl<T> Sync for Handle<T> where T: Sync {}

impl<T> Handle<T> {
    /// # Safety
    /// The arena this handle comes from must still be alive.
    #[inline(always)]
    pub unsafe fn as_slice(&self) -> &[MaybeUninit<T>] {
        match &self.0 {
            // SAFETY: the arena is alive, as stabilished by the method contract
            HandleInner::Arena { ptr, .. } => unsafe { ptr.as_ref() },
            HandleInner::Boxed(b) => b,
        }
    }

    /// # Safety
    /// The arena this handle comes from must still be alive.
    #[inline(always)]
    pub unsafe fn as_mut_slice(&mut self) -> &mut [MaybeUninit<T>] {
        match &mut self.0 {
            // SAFETY: the arena is alive, as stabilished by the method contract
            HandleInner::Arena { ptr, .. } => unsafe { ptr.as_mut() },
            HandleInner::Boxed(b) => b,
        }
    }

    /// Whether this handle actually contains a boxed value.
    pub fn is_boxed(&self) -> bool {
        match self.0 {
            HandleInner::Arena { .. } => false,
            HandleInner::Boxed(_) => true,
        }
    }
}

impl<T> Drop for Handle<T> {
    fn drop(&mut self) {
        match &mut self.0 {
            HandleInner::Arena { ptr, sender } => {
                _ = sender.send(*ptr);
            }
            HandleInner::Boxed(_) => (),
        }
    }
}

/// Arena for short-lived objects.
pub struct RingArena<T> {
    storage: Box<[MaybeUninit<T>]>,
    allocations: VecDeque<ArenaAllocation>,

    sender: Sender<NonNull<[MaybeUninit<T>]>>,
    receiver: Receiver<NonNull<[MaybeUninit<T>]>>,
}

impl<T> RingArena<T> {
    pub fn new(capacity: usize) -> Self {
        let (sender, receiver) = flume::unbounded();
        let mut storage = Vec::with_capacity(capacity);
        // SAFETY: MaybeUninit is always considered initialized
        unsafe { storage.set_len(capacity) };

        Self {
            storage: storage.into_boxed_slice(),
            allocations: VecDeque::new(),

            sender,
            receiver,
        }
    }

    #[inline(always)]
    fn left_index(&self) -> usize {
        let Some(first) = self.allocations.front() else {
            return self.storage.len();
        };

        first.start
    }

    #[inline(always)]
    fn available_left(&self) -> usize {
        self.left_index()
    }

    #[inline(always)]
    fn right_index(&self) -> usize {
        let Some(last) = self.allocations.back() else {
            return self.storage.len();
        };

        last.end()
    }

    #[inline(always)]
    fn available_right(&self) -> usize {
        self.storage.len() - self.right_index()
    }

    fn deallocate(&mut self) {
        while let Ok(alloc) = self.receiver.try_recv() {
            let start = unsafe {
                alloc
                    .as_ptr()
                    .cast::<MaybeUninit<T>>()
                    .offset_from_unsigned(self.storage.as_ptr())
            };

            let index = self
                .allocations
                .binary_search_by_key(&start, |k| k.start)
                .expect("allocation must be in the list");

            let elem = self.allocations.remove(index).unwrap();
            assert_eq!(elem.len, alloc.len());
        }
    }

    pub fn allocate(&mut self, length: usize) -> Handle<T> {
        self.deallocate();
        if self.available_right() >= length {
            let start = self.right_index();
            let handle = Handle(HandleInner::Arena {
                ptr: NonNull::slice_from_raw_parts(
                    NonNull::from_mut(&mut self.storage[start]),
                    length,
                ),
                sender: self.sender.clone(),
            });

            self.allocations
                .push_back(ArenaAllocation { start, len: length });

            handle
        } else if self.available_left() >= length {
            let start = self.left_index() - length;
            let handle = Handle(HandleInner::Arena {
                ptr: NonNull::slice_from_raw_parts(
                    NonNull::from_mut(&mut self.storage[start]),
                    length,
                ),
                sender: self.sender.clone(),
            });

            self.allocations
                .push_front(ArenaAllocation { start, len: length });

            handle
        } else {
            // fallback - allocate on the heap
            Handle(HandleInner::Boxed(Box::new_uninit_slice(length)))
        }
    }
}

unsafe impl<T> Send for RingArena<T> where T: Send {}
unsafe impl<T> Sync for RingArena<T> where T: Sync {}

impl<T> Drop for RingArena<T> {
    fn drop(&mut self) {
        if !self.allocations.is_empty() {
            panic!("ring arena dropped while allocations exist");
        }
    }
}

#[cfg(test)]
mod test {
    use crate::RingArena;

    #[test]
    fn test() {
        let mut arena = RingArena::<u32>::new(4);
        let a = arena.allocate(1);
        let b = arena.allocate(3);
        let c = arena.allocate(2);

        assert_eq!(unsafe { a.as_slice().len() }, 1);
        assert_eq!(unsafe { b.as_slice().len() }, 3);
        assert_eq!(unsafe { c.as_slice().len() }, 2);
        assert!(!a.is_boxed());
        assert!(!b.is_boxed());
        assert!(c.is_boxed());

        std::mem::drop((a, b));
        let d = arena.allocate(4);
        assert_eq!(unsafe { d.as_slice().len() }, 4);
        assert!(!d.is_boxed());
    }
}
