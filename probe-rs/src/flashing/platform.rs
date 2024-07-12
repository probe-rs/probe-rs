//! Traits for platform-specific firmware loading.

use crate::{
    flashing::{FileDownloadError, FlashLoader, Format, ImageLoader, ImageReader},
    vendor::espressif::platform::IdfPlatform,
    Session,
};

/// Helper trait to allow cloning the platform object.
#[doc(hidden)]
pub trait ClonePlatform {
    /// Clones the platform object.
    fn clone_box(&self) -> Box<dyn PlatformImpl>;
}

impl<T> ClonePlatform for T
where
    T: PlatformImpl + Clone + 'static,
{
    fn clone_box(&self) -> Box<dyn PlatformImpl> {
        Box::new(self.clone())
    }
}

/// Helper trait to allow cloning the platform loader object.
#[doc(hidden)]
pub trait ClonePlatformLoader {
    /// Clones the platform loader object.
    fn clone_box(&self) -> Box<dyn PlatformImageLoader>;
}

impl<T> ClonePlatformLoader for T
where
    T: PlatformImageLoader + Clone + 'static,
{
    fn clone_box(&self) -> Box<dyn PlatformImageLoader> {
        Box::new(self.clone())
    }
}

/// A platform.
///
/// Implementors must be `Clone`.
pub trait PlatformImpl: ClonePlatform {
    /// Loads and processes the firmware.
    fn default_loader(&self) -> PlatformLoader;
}

/// The platform-specific implementation of the firmware loading process.
///
/// Implementors must be `Clone`.
pub trait PlatformImageLoader: ClonePlatformLoader {
    /// Loads and processes the firmware.
    fn load(
        &self,
        flash_loader: &mut FlashLoader,
        format: Format,
        session: &mut Session,
        file: &mut dyn ImageReader,
    ) -> Result<(), FileDownloadError>;
}

/// A finite list of all the available platforms probe-rs understands.
pub struct Platform(Box<dyn PlatformImpl>);

impl Default for Platform {
    fn default() -> Self {
        Self::from(RawPlatform)
    }
}

impl Clone for Platform {
    fn clone(&self) -> Self {
        Self(self.0.clone_box())
    }
}

impl<T> From<T> for Platform
where
    T: PlatformImpl + 'static,
{
    fn from(platform: T) -> Self {
        Self(Box::new(platform))
    }
}

impl Platform {
    /// Tries to parse a string into a platform.
    pub fn from_optional(s: Option<&str>) -> Option<Result<Self, String>> {
        let result = match s? {
            "raw" => Ok(Self::from(RawPlatform)),
            "idf" | "esp-idf" | "espidf" => Ok(Self::from(IdfPlatform)),
            other => Err(format!("Platform '{other}' is unknown.")),
        };

        Some(result)
    }

    /// Returns the default image loader for the given platform.
    pub fn default_loader(&self) -> PlatformLoader {
        self.0.default_loader()
    }
}

/// A finite list of all the available platforms probe-rs understands.
pub struct PlatformLoader(Box<dyn PlatformImageLoader>);

impl Clone for PlatformLoader {
    fn clone(&self) -> Self {
        Self(self.0.clone_box())
    }
}

impl<T> From<T> for PlatformLoader
where
    T: PlatformImageLoader + 'static,
{
    fn from(platform: T) -> Self {
        Self(Box::new(platform))
    }
}

impl PlatformLoader {
    /// Loads and processes the firmware.
    pub fn load(
        &self,
        flash_loader: &mut FlashLoader,
        format: Format,
        session: &mut Session,
        file: &mut dyn ImageReader,
    ) -> Result<(), FileDownloadError> {
        self.0.load(flash_loader, format, session, file)
    }
}

/// The firmware does not need any special handling.
#[derive(Clone)]
pub struct RawPlatform;

/// The firmware does not need any special handling.
#[derive(Clone)]
pub struct RawLoader;

impl PlatformImpl for RawPlatform {
    fn default_loader(&self) -> PlatformLoader {
        PlatformLoader::from(RawLoader)
    }
}

impl PlatformImageLoader for RawLoader {
    fn load(
        &self,
        flash_loader: &mut FlashLoader,
        format: Format,
        _session: &mut Session,
        file: &mut dyn ImageReader,
    ) -> Result<(), FileDownloadError> {
        format.load(flash_loader, file)
    }
}
