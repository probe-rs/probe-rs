/// Temporary macro for skipping tests.
///
/// This should be done by marking the test as `#[ignore]` ,
/// until skipping at runtime gets possible.
#[macro_export]
macro_rules! skip_test {
    ($reason:expr) => {
        println!("Skipping test: {}", $reason);
        return Ok(());
    };
    () => {
        return Ok(());
    };
}
