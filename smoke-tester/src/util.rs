struct CoreTestCase {
    name: String,

    test_fn: F,
}

enum TestResult {
    Ok,
    Skipped,
    Failed,
}
