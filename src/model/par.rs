//! Per-channel parallelism with `std::thread::scope`.
//!
//! Channels are independent in every whole-file operation that uses this (pyramid build,
//! resample), so the work splits by channel with no shared state. Threads are capped at
//! `available_parallelism`; each thread takes a contiguous run of channels, so the output keeps
//! the input's order.

/// The number of worker threads to use for `items` independent jobs.
pub fn worker_count(items: usize) -> usize {
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    cores.min(items).max(1)
}

/// `items.iter().map(f).collect()`, with the items split across worker threads.
pub fn map_channels<T, R, F>(items: &[T], f: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> R + Sync,
{
    let workers = worker_count(items.len());
    if workers <= 1 {
        return items.iter().map(&f).collect();
    }
    let per_worker = items.len().div_ceil(workers);
    let f = &f;
    std::thread::scope(|scope| {
        let handles: Vec<_> = items
            .chunks(per_worker)
            .map(|chunk| scope.spawn(move || chunk.iter().map(f).collect::<Vec<R>>()))
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_input_order() {
        let items: Vec<usize> = (0..103).collect();
        let out = map_channels(&items, |&i| i * 2);
        assert_eq!(out, items.iter().map(|i| i * 2).collect::<Vec<_>>());
    }

    #[test]
    fn handles_empty_and_single() {
        assert!(map_channels::<usize, usize, _>(&[], |&i| i).is_empty());
        assert_eq!(map_channels(&[7usize], |&i| i + 1), vec![8]);
    }
}
