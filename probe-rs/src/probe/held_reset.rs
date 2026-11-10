//! nRESET held asserted across the steps of an attach under reset.

use std::ops::{Deref, DerefMut};
use std::time::Duration;

use crate::Error;
use crate::architecture::arm::{ArmDebugInterface, Pins};
use crate::probe::Probe;

/// How nRESET was asserted, which is how it has to be released.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResetAssertion {
    /// Through the debug port's pins.
    Pins,
    /// Through the probe's own reset command.
    Command,
}

/// Something that can release nRESET: a probe, or the ARM interface made from it.
pub(crate) trait ReleaseReset {
    fn release_reset(&mut self, assertion: ResetAssertion) -> Result<(), Error>;
}

/// A probe, or the ARM interface made from it, while an attach under reset holds nRESET.
///
/// Dropping it releases nRESET. An attach that fails before it releases the line itself returns
/// through this drop, so the target is not left held in reset. [`HeldReset::into_inner`] ends the
/// hold.
pub(crate) struct HeldReset<T: ReleaseReset> {
    /// `None` only inside [`HeldReset::into_inner`] and [`HeldReset::try_map`].
    inner: Option<T>,
    /// `None` when nothing was asserted, as in an attach that is not under reset.
    assertion: Option<ResetAssertion>,
}

impl<T: ReleaseReset> HeldReset<T> {
    pub(crate) fn new(inner: T, assertion: Option<ResetAssertion>) -> Self {
        Self {
            inner: Some(inner),
            assertion,
        }
    }

    /// Ends the hold: from here on, releasing nRESET is the caller's job.
    pub(crate) fn into_inner(mut self) -> T {
        self.inner.take().expect("present until the hold ends")
    }

    /// Turns the held value into another, keeping the hold. A failing `f` hands the value back,
    /// and the line is released through it.
    pub(crate) fn try_map<U: ReleaseReset, E>(
        mut self,
        f: impl FnOnce(T) -> Result<U, (T, E)>,
    ) -> Result<HeldReset<U>, E> {
        let assertion = self.assertion;
        let inner = self.inner.take().expect("present until the hold ends");
        match f(inner) {
            Ok(inner) => Ok(HeldReset::new(inner, assertion)),
            Err((inner, error)) => {
                drop(HeldReset::new(inner, assertion));
                Err(error)
            }
        }
    }
}

impl<T: ReleaseReset> Deref for HeldReset<T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.inner.as_ref().expect("present until the hold ends")
    }
}

impl<T: ReleaseReset> DerefMut for HeldReset<T> {
    fn deref_mut(&mut self) -> &mut T {
        self.inner.as_mut().expect("present until the hold ends")
    }
}

impl<T: ReleaseReset> Drop for HeldReset<T> {
    fn drop(&mut self) {
        if let (Some(inner), Some(assertion)) = (self.inner.as_mut(), self.assertion)
            && let Err(error) = inner.release_reset(assertion)
        {
            // The error that ended the attach is the one to report.
            tracing::warn!("Unable to release the reset line: {error}");
        }
    }
}

fn nreset_high() -> Pins {
    let mut nreset = Pins(0);
    nreset.set_nreset(true);
    nreset
}

impl ReleaseReset for Probe {
    fn release_reset(&mut self, assertion: ResetAssertion) -> Result<(), Error> {
        match assertion {
            ResetAssertion::Pins => self.with_debug_port_wire(|wire| {
                wire.swj_pins(nreset_high(), nreset_high(), Duration::ZERO)
                    .map(drop)
            })?,
            ResetAssertion::Command => self.target_reset_deassert()?,
        }
        Ok(())
    }
}

impl<T: ReleaseReset + ?Sized> ReleaseReset for &mut T {
    fn release_reset(&mut self, assertion: ResetAssertion) -> Result<(), Error> {
        (**self).release_reset(assertion)
    }
}

impl ReleaseReset for Box<dyn ArmDebugInterface + '_> {
    fn release_reset(&mut self, assertion: ResetAssertion) -> Result<(), Error> {
        if assertion == ResetAssertion::Command
            && let Some(driver) = self.driver_mut()
        {
            return Ok(driver.target_reset_deassert()?);
        }
        let nreset = u32::from(nreset_high().0);
        self.swj_pins(nreset, nreset, 0)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::{Error, HeldReset, ReleaseReset, ResetAssertion};

    /// Records each release. `fail` makes a release fail, as a probe that lost USB would.
    struct Line<'a> {
        releases: &'a RefCell<Vec<ResetAssertion>>,
        fail: bool,
    }

    impl ReleaseReset for Line<'_> {
        fn release_reset(&mut self, assertion: ResetAssertion) -> Result<(), Error> {
            self.releases.borrow_mut().push(assertion);
            if self.fail {
                return Err(Error::Other("gone".into()));
            }
            Ok(())
        }
    }

    fn line(releases: &RefCell<Vec<ResetAssertion>>) -> Line<'_> {
        Line {
            releases,
            fail: false,
        }
    }

    #[test]
    fn an_error_before_the_hold_ends_releases_the_way_it_asserted() {
        let releases = RefCell::new(vec![]);
        // A failing release is only logged: the attach error is the one returned.
        let attach = || -> Result<(), ()> {
            let _held = HeldReset::new(
                Line {
                    releases: &releases,
                    fail: true,
                },
                Some(ResetAssertion::Command),
            );
            Err(())
        };
        assert!(attach().is_err());
        assert_eq!(*releases.borrow(), [ResetAssertion::Command]);
    }

    #[test]
    fn nothing_asserted_nothing_released() {
        let releases = RefCell::new(vec![]);
        drop(HeldReset::new(line(&releases), None));
        assert!(releases.borrow().is_empty());
    }

    #[test]
    fn a_map_keeps_the_hold() {
        let first = RefCell::new(vec![]);
        let held = HeldReset::new(line(&first), Some(ResetAssertion::Pins));
        let mapped: Result<HeldReset<Line<'_>>, ()> = held.try_map(|line| Err((line, ())));
        assert!(mapped.is_err());
        assert_eq!(*first.borrow(), [ResetAssertion::Pins]);

        let second = RefCell::new(vec![]);
        let held = HeldReset::new(line(&first), Some(ResetAssertion::Pins));
        drop(held.try_map(|_| Ok::<_, (Line<'_>, ())>(line(&second))));
        assert_eq!(*second.borrow(), [ResetAssertion::Pins]);
    }
}
