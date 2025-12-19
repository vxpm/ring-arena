use std::{collections::VecDeque, mem::MaybeUninit, num::NonZero, ptr::NonNull};
use triomphe::Arc;

enum HandleInner<T> {
    Chunk {
        ptr: NonNull<[MaybeUninit<T>]>,
        _count: Arc<()>,
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
            HandleInner::Chunk { ptr, .. } => unsafe { ptr.as_ref() },
            HandleInner::Boxed(b) => b,
        }
    }

    /// # Safety
    /// The arena this handle comes from must still be alive.
    #[inline(always)]
    pub unsafe fn as_mut_slice(&mut self) -> &mut [MaybeUninit<T>] {
        match &mut self.0 {
            // SAFETY: the arena is alive, as stabilished by the method contract
            HandleInner::Chunk { ptr, .. } => unsafe { ptr.as_mut() },
            HandleInner::Boxed(b) => b,
        }
    }

    /// Whether this handle actually contains a boxed value.
    pub fn is_boxed(&self) -> bool {
        match self.0 {
            HandleInner::Chunk { .. } => false,
            HandleInner::Boxed(_) => true,
        }
    }
}

struct Chunk<T> {
    storage: Box<[MaybeUninit<T>]>,
    allocated: Arc<()>,
}

impl<T> Chunk<T> {
    pub fn new(length: usize) -> Self {
        let mut storage = Vec::with_capacity(length);
        // SAFETY: MaybeUninit is always considered initialized
        unsafe { storage.set_len(length) };

        Self {
            storage: storage.into_boxed_slice(),
            allocated: Arc::new(()),
        }
    }
}

/// Arena for short-lived objects.
pub struct RingArena<T> {
    chunk_length: usize,
    /// All the allocated chunks.
    chunks: VecDeque<Chunk<T>>,
    /// Offset into the front chunk.
    offset: usize,
}

impl<T> RingArena<T> {
    pub fn new(chunk_length: NonZero<usize>) -> Self {
        let first = Chunk::new(chunk_length.get());
        Self {
            chunk_length: chunk_length.get(),
            chunks: VecDeque::from([first]),
            offset: 0,
        }
    }

    /// # Safety
    /// `length` elements must fit within the remaining space of the front chunk.
    unsafe fn allocate_unchecked(&mut self, length: usize) -> Handle<T> {
        let front = self.chunks.front_mut().unwrap();
        let handle = Handle(HandleInner::Chunk {
            ptr: NonNull::slice_from_raw_parts(
                NonNull::from_mut(&mut front.storage[self.offset]),
                length,
            ),
            _count: front.allocated.clone(),
        });

        self.offset += length;
        handle
    }

    pub fn allocate(&mut self, length: usize) -> Handle<T> {
        if length == 0 || length > self.chunk_length {
            return Handle(HandleInner::Boxed(Box::new_uninit_slice(length)));
        }

        let remaining = self.chunk_length - self.offset;
        if remaining >= length {
            unsafe { self.allocate_unchecked(length) }
        } else {
            // chunk is full, move front chunk to the back
            let full = self.chunks.pop_front().unwrap();
            self.chunks.push_back(full);
            self.offset = 0;

            let front = self.chunks.front_mut().unwrap();
            if front.allocated.is_unique() {
                unsafe { self.allocate_unchecked(length) }
            } else {
                let chunk = Chunk::new(self.chunk_length);
                self.chunks.push_front(chunk);
                unsafe { self.allocate_unchecked(length) }
            }
        }
    }
}

unsafe impl<T> Send for RingArena<T> where T: Send {}
unsafe impl<T> Sync for RingArena<T> where T: Sync {}

impl<T> Drop for RingArena<T> {
    fn drop(&mut self) {
        for chunk in &self.chunks {
            if !chunk.allocated.is_unique() {
                panic!("ring arena dropped while allocations exist");
            }
        }
    }
}
