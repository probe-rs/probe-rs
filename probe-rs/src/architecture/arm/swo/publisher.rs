use serde::{Deserialize, Serialize};
use std::sync::mpsc::{Receiver, Sender};

pub trait SwoPublisher {
    /// Starts the `SwoPublisher`.
    /// This should never block and run the `Updater` asynchronously.
    fn start<
        I: Serialize + Send + Sync + 'static,
        O: Deserialize<'static> + Send + Sync + 'static,
    >(
        &mut self,
    ) -> UpdaterChannel<I, O>;
    /// Stops the `SwoPublisher` if currently running.
    /// Returns `Ok` if everything went smooth during the run of the `SwoPublisher`.
    /// Returns `Err` if something went wrong during the run of the `SwoPublisher`.
    fn stop(&mut self) -> Result<(), ()>;
}

/// A complete channel to an updater.
/// Rx and tx naming is done from the user view of the channel, not the `Updater` view.
pub struct UpdaterChannel<
    I: Serialize + Send + Sync + 'static,
    O: Deserialize<'static> + Send + Sync + 'static,
> {
    /// The rx where the user reads data from.
    rx: Receiver<O>,
    /// The tx where the user sends data to.
    tx: Sender<I>,
}

impl<I: Serialize + Send + Sync + 'static, O: Deserialize<'static> + Send + Sync + 'static>
    UpdaterChannel<I, O>
{
    /// Creates a new `UpdaterChannel` where crossover is done internally.
    /// The argument naming is done from the `Updater`s view. Where as the member naming is done from a user point of view.
    pub fn new(rx: Sender<I>, tx: Receiver<O>) -> Self {
        Self { rx: tx, tx: rx }
    }

    /// Returns the rx end of the channel.
    pub fn rx(&mut self) -> &mut Receiver<O> {
        &mut self.rx
    }

    /// Returns the tx end of the channel.
    pub fn tx(&mut self) -> &mut Sender<I> {
        &mut self.tx
    }
}
