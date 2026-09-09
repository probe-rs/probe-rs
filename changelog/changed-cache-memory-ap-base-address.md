The ARM debug interface now caches memory AP base addresses instead of re-reading them from the target on every access, since they never change.
