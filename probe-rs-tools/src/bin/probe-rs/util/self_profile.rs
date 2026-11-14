//! Profiles probe-rs itself, selected with environment variables:
//!
//! - `PROBE_RS_CHROME_TRACE=<file>` writes the spans as a Chrome trace, for Perfetto.
//! - `PROBE_RS_ALLOC_STATS=<file>` writes the heap allocations of each span name at exit.

pub use imp::{ProfileGuard, layers};

#[cfg(feature = "self-profile")]
mod imp {
    use std::{
        alloc::{GlobalAlloc, Layout, System},
        cell::{Cell, RefCell},
        collections::HashMap,
        ffi::OsString,
        fs::File,
        io::Write,
        ops::{AddAssign, Sub},
        sync::Arc,
    };

    use anyhow::Context;
    use parking_lot::Mutex;
    use tracing::{Subscriber, span::Id};
    use tracing_chrome::{ChromeLayerBuilder, FlushGuard};
    use tracing_subscriber::{
        Layer, Registry, filter::filter_fn, layer::Context as LayerContext, registry::LookupSpan,
    };

    use crate::util::logging::eprintln;

    #[global_allocator]
    static ALLOCATOR: CountingAllocator = CountingAllocator;

    #[derive(Clone, Copy, Default)]
    struct Counts {
        allocs: u64,
        bytes: u64,
    }

    impl Sub for Counts {
        type Output = Self;

        fn sub(self, other: Self) -> Self {
            Self {
                allocs: self.allocs - other.allocs,
                bytes: self.bytes - other.bytes,
            }
        }
    }

    impl AddAssign for Counts {
        fn add_assign(&mut self, other: Self) {
            self.allocs += other.allocs;
            self.bytes += other.bytes;
        }
    }

    struct Frame {
        start: Counts,
        children: Counts,
    }

    thread_local! {
        static COUNTS: Cell<Counts> = const { Cell::new(Counts { allocs: 0, bytes: 0 }) };
        static STACK: RefCell<Vec<Frame>> = const { RefCell::new(Vec::new()) };
    }

    fn counts() -> Counts {
        COUNTS.try_with(Cell::get).unwrap_or_default()
    }

    /// Counts the allocations of each thread. A reallocation counts as one allocation of the
    /// new size.
    struct CountingAllocator;

    impl CountingAllocator {
        fn count(bytes: usize) {
            // The thread-local is gone while its thread exits.
            let _ = COUNTS.try_with(|counts| {
                let mut new = counts.get();
                new.allocs += 1;
                new.bytes += bytes as u64;
                counts.set(new);
            });
        }
    }

    // SAFETY: each method forwards to `System` with the same arguments.
    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            Self::count(layout.size());
            unsafe { System.alloc(layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            Self::count(layout.size());
            unsafe { System.alloc_zeroed(layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            Self::count(new_size);
            unsafe { System.realloc(ptr, layout, new_size) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    #[derive(Default)]
    struct SpanAllocs {
        spans: u64,
        total: Counts,
        own: Counts,
    }

    type Totals = Arc<Mutex<HashMap<&'static str, SpanAllocs>>>;

    /// Adds the allocations between the enter and the exit of each span to the totals of its
    /// name. The own allocations leave out the allocations of the child spans.
    struct AllocLayer {
        totals: Totals,
    }

    impl<S> Layer<S> for AllocLayer
    where
        S: Subscriber + for<'span> LookupSpan<'span>,
    {
        fn on_enter(&self, _id: &Id, _ctx: LayerContext<'_, S>) {
            let _ = STACK.try_with(|stack| {
                let mut stack = stack.borrow_mut();
                stack.push(Frame {
                    start: Counts::default(),
                    children: Counts::default(),
                });
                // Read the counts after the push, so that a growth of the stack counts for the
                // parent span.
                if let Some(frame) = stack.last_mut() {
                    frame.start = counts();
                }
            });
        }

        fn on_exit(&self, id: &Id, ctx: LayerContext<'_, S>) {
            let now = counts();
            let Ok(Some(frame)) = STACK.try_with(|stack| {
                let mut stack = stack.borrow_mut();
                let frame = stack.pop()?;
                if let Some(parent) = stack.last_mut() {
                    parent.children += now - frame.start;
                }
                Some(frame)
            }) else {
                return;
            };
            let Some(metadata) = ctx.metadata(id) else {
                return;
            };

            let total = now - frame.start;
            let mut totals = self.totals.lock();
            let entry = totals.entry(metadata.name()).or_default();
            entry.spans += 1;
            entry.total += total;
            entry.own += total - frame.children;
        }
    }

    /// Writes the allocation totals when dropped.
    struct AllocReport {
        path: OsString,
        file: File,
        totals: Totals,
    }

    impl AllocReport {
        fn write(&mut self) -> std::io::Result<()> {
            let totals = self.totals.lock();
            let mut rows: Vec<_> = totals.iter().collect();
            rows.sort_by_key(|(_, allocs)| std::cmp::Reverse(allocs.own.bytes));

            writeln!(
                self.file,
                "{:<32} {:>8} {:>10} {:>12} {:>10} {:>12} {:>9}",
                "span", "count", "allocs", "bytes", "own allocs", "own bytes", "own/span"
            )?;
            for (name, allocs) in rows {
                writeln!(
                    self.file,
                    "{:<32} {:>8} {:>10} {:>12} {:>10} {:>12} {:>9.1}",
                    name,
                    allocs.spans,
                    allocs.total.allocs,
                    allocs.total.bytes,
                    allocs.own.allocs,
                    allocs.own.bytes,
                    allocs.own.allocs as f64 / allocs.spans as f64,
                )?;
            }

            Ok(())
        }
    }

    impl Drop for AllocReport {
        fn drop(&mut self) {
            if let Err(error) = self.write() {
                eprintln(format!(
                    "Warning: Unable to write the allocation counts to {:?}: {error}",
                    self.path
                ));
            }
        }
    }

    /// Keeps the profile outputs open, and writes them when dropped.
    #[derive(Default)]
    pub struct ProfileGuard {
        _chrome_trace: Option<FlushGuard>,
        _alloc_report: Option<AllocReport>,
    }

    fn create(variable: &str) -> anyhow::Result<Option<(OsString, File)>> {
        let Some(path) = std::env::var_os(variable) else {
            return Ok(None);
        };
        let file = File::create(&path)
            .with_context(|| format!("Unable to create the file {path:?} from {variable}"))?;

        Ok(Some((path, file)))
    }

    /// Returns the layers that the environment variables select.
    pub fn layers() -> (Vec<Box<dyn Layer<Registry> + Send + Sync>>, ProfileGuard) {
        let mut layers = Vec::new();
        let mut guard = ProfileGuard::default();

        match create("PROBE_RS_CHROME_TRACE") {
            Ok(Some((_, file))) => {
                let (layer, flush) = ChromeLayerBuilder::new()
                    .writer(file)
                    .include_args(true)
                    .build();
                // Each trace event would become an instant event, which makes the file very
                // large.
                layers.push(layer.with_filter(filter_fn(|meta| meta.is_span())).boxed());
                guard._chrome_trace = Some(flush);
            }
            Ok(None) => {}
            Err(error) => eprintln(format!("Warning: {error:#}. Running without a trace.")),
        }

        match create("PROBE_RS_ALLOC_STATS") {
            Ok(Some((path, file))) => {
                if guard._chrome_trace.is_some() {
                    eprintln(
                        "Warning: The allocation counts include the allocations of the Chrome trace.",
                    );
                }
                // A new name after this capacity makes the map allocate in `on_exit`, and that
                // allocation counts for the parent span.
                let totals = Arc::new(Mutex::new(HashMap::with_capacity(256)));
                let layer = AllocLayer {
                    totals: totals.clone(),
                };
                layers.push(layer.with_filter(filter_fn(|meta| meta.is_span())).boxed());
                guard._alloc_report = Some(AllocReport { path, file, totals });
            }
            Ok(None) => {}
            Err(error) => eprintln(format!(
                "Warning: {error:#}. Running without allocation counts."
            )),
        }

        (layers, guard)
    }
}

#[cfg(not(feature = "self-profile"))]
mod imp {
    use tracing_subscriber::{Layer, Registry};

    #[derive(Default)]
    pub struct ProfileGuard;

    pub fn layers() -> (Vec<Box<dyn Layer<Registry> + Send + Sync>>, ProfileGuard) {
        (Vec::new(), ProfileGuard)
    }
}
