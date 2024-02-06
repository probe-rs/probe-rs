/// The `Permissions` struct represents what a [`Session`](crate::Session) is allowed to do with a target.
/// Some operations can be irreversable, so need to be explicitly allowed by the user.
///
/// # Example
///
/// ```
/// use probe_rs::Permissions;
///
/// let permissions = Permissions::new().allow_erase_all();
/// ```
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct Permissions {
    /// When set to true, all memory of the chip may be erased or reset to factory default
    erase_all: bool,
}

impl Permissions {
    /// Constructs a new permissions object with the default values
    pub fn new() -> Self {
        Self::default()
    }

    /// Allow the session to erase all memory of the chip or reset it to factory default.
    ///
    /// # Warning
    /// This may irreversibly remove otherwise read-protected data from the device like security keys and 3rd party firmware.
    /// What happens exactly may differ per device and per probe-rs version.
    #[must_use]
    pub fn allow_erase_all(self) -> Self {
        Self {
            erase_all: true,
            ..self
        }
    }

    pub(crate) fn erase_all(&self) -> Result<(), MissingPermissions> {
        if self.erase_all {
            Ok(())
        } else {
            Err(MissingPermissions {
                operation: "erase_all".into(),
            })
        }
    }
}

#[derive(Debug, Clone, thiserror::Error)]
#[error(
    "An operation could not be performed because it lacked the permission to do so: {operation}"
)]
pub struct MissingPermissions {
    pub operation: String,
}
