/// Removes all arguments from the commandline input that `cargo build` does not understand.
/// All the arguments are removed in place!
/// It expects a valid list of arguments to be removed. If the argument can have a value it MUST contain a `=` at the end.
/// The validity of the arguments list can be ensured by having `structopt` parse the arguments first and check its result.
/// E.g:
/// ```rust
/// let arguments_to_remove = [
///     "foo", // Can be "--foo"
///     "bar=", // Can be "--bar=value" and "--bar value"
/// ];
/// ```
pub fn remove_arguments(arguments_to_remove: &[&'static str], arguments: &mut Vec<String>) {
    // We iterate all arguments that possibly have to be removed
    // and remove them if they occur to be in the input.
    for argument in arguments_to_remove.iter() {
        // Make sure the compared against arg does not contain an equal sign.
        // If the original arg contained an equal sign we take this as a hint
        // that the arg can be used as `--arg value` as well as `--arg=value`.
        // In the prior case we need to remove two arguments. So remember this.
        let (remove_two, clean_argument) = if let Some(stripped) = argument.strip_suffix('=') {
            (true, format!("--{}", stripped))
        } else {
            (false, format!("--{}", argument))
        };

        // Iterate all args in the input and if we find one that matches, we remove it.
        if let Some(index) = arguments
            .iter()
            .position(|x| x.starts_with(&format!("--{}", argument)))
        {
            // We remove the argument we found.
            arguments.remove(index);
        }

        // If the argument requires a value we also need to check for the case where no
        // = (equal sign) was present, in which case the value is a second argument
        // which we need to remove as well.
        if remove_two {
            // Iterate all args in the input and if we find one that matches, we remove it.
            if let Some(index) = arguments.iter().position(|x| x == &clean_argument) {
                // We remove the argument we found plus its value.
                arguments.remove(index);
                arguments.remove(index);
            }
        }
    }
}

#[test]
/// This test will test that all arguments are properly removed.
/// The [remove_arguments] function only works if the arguments are valid.
/// In real world applications this will always hold true because `structopt` which we have infront of this removal
/// will always ensure that the arguments are valid and in correct order!
fn remove_arguments_test() {
    let arguments_to_remove = [
        "chip=",
        "chip-description-path=",
        "list-chips",
        "disable-progressbars",
        "protocol=",
        "probe-index=",
        "gdb",
        "no-download",
        "reset-halt",
        "gdb-connection-string=",
        "nrf-recover",
    ];

    let mut arguments = vec![
        "--chip-description-path=kek".to_string(),
        "--chip-description-path".to_string(),
        "kek".to_string(),
        "--chip=kek".to_string(),
        "--chip".to_string(),
        "kek".to_string(),
        "--list-chips".to_string(),
        "--disable-progressbars".to_string(),
        "--protocol=kek".to_string(),
        "--protocol".to_string(),
        "kek".to_string(),
        "--probe-index=kek".to_string(),
        "--probe-index".to_string(),
        "kek".to_string(),
        "--gdb".to_string(),
        "--no-download".to_string(),
        "--reset-halt".to_string(),
        "--gdb-connection-string=kek".to_string(),
        "--gdb-connection-string".to_string(),
        "kek".to_string(),
        "--nrf-recover".to_string(),
    ];

    remove_arguments(&arguments_to_remove, &mut arguments);

    println!("{:?}", arguments);

    assert!(arguments.is_empty());
}
