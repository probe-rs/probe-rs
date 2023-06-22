/// Notes about SVD:
/// - Peripherals are 'grouped', but many only belong to a single group.
/// - We only have to build the structure once down to 'fields' level.
/// - Once an SVD file has been parsed, it's structure is loaded as a hierarchical set of variables.
/// - Fields need to be read every stacktrace, because they will change value.
// TODO: Implement 'lazy load' of registers, to only read target registers for peripherals that are expanded in the VSCode variable view.
pub(crate) mod svd_variables;
