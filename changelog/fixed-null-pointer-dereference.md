The debugger no longer reads the memory that a pointer refers to when the pointer holds zero, or holds the alignment of the type, as `core::ptr::NonNull::dangling` creates for an empty collection.
