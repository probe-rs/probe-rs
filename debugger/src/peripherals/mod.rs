/// Notes about SVD:
/// Peripherals:
/// - Are 'grouped', but many only belong to a single group.
/// - The 'derived from' properties need to be read first, then overlay the specified properties
/// Start off with everything being read-only.
/// We only have to build the structure once down to 'fields' level.
/// Fields need to be read every stacktrace, because they will change value.
/// Once an SVD file has been parsed, it's structure is loaded as a hierarchical set of variables.
pub(crate) mod svd_variables;
