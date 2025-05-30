//! Types for emitting `$/progress` notifications to the client.

use std::fmt::{self, Debug, Formatter};
use std::marker::PhantomData;

use super::Client;

/// Indicates the progress stream is bounded from 0-100%.
#[doc(hidden)]
#[derive(Debug)]
pub enum Bounded {}

/// Indicates the progress stream is unbounded.
#[doc(hidden)]
#[derive(Debug)]
pub enum Unbounded {}

/// Indicates the progress stream may be canceled by the client.
#[doc(hidden)]
#[derive(Debug)]
pub enum Cancellable {}

/// Indicates the progress stream cannot be canceled by the client.
#[doc(hidden)]
#[derive(Debug)]
pub enum NotCancellable {}

/// A builder for a new `$/progress` stream.
///
/// This progress stream is initially assumed to be _unbounded_ and _not cancellable_.
///
/// This struct is created by [`Client::progress`]. See its documentation for more.
#[must_use = "progress is not reported until `.begin()` is called"]
pub struct Progress<B = Unbounded, C = NotCancellable> {
    client: Client,
    token: ProgressToken,
    begin_msg: WorkDoneProgressBegin,
    _kind: PhantomData<(B, C)>,
}

impl Progress {
    pub(crate) fn new(client: Client, token: ProgressToken, title: String) -> Self {
        Progress {
            client,
            token,
            begin_msg: WorkDoneProgressBegin {
                title,
                cancellable: Some(false),
                message: None,
                percentage: None,
            },
            _kind: PhantomData,
        }
    }
}

impl<C> Progress<Unbounded, C> {
    /// Sets the optional progress percentage to display in the client UI.
    ///
    /// This percentage value is initially `start_percentage`, where a value of `100` for example
    /// is considered 100% by the client. If this method is not called, unbounded progress is
    /// assumed.
    pub fn with_percentage(self, start_percentage: u32) -> Progress<Bounded, C> {
        Progress {
            client: self.client,
            token: self.token,
            begin_msg: WorkDoneProgressBegin {
                percentage: Some(start_percentage),
                ..self.begin_msg
            },
            _kind: PhantomData,
        }
    }
}

impl<B> Progress<B, NotCancellable> {
    /// Indicates that a "cancel" button should be displayed in the client UI.
    ///
    /// Clients that don’t support cancellation are allowed to ignore this setting. If this method
    /// is not called, the user will not be presented with an option to cancel this operation.
    pub fn with_cancel_button(self) -> Progress<B, Cancellable> {
        Progress {
            client: self.client,
            token: self.token,
            begin_msg: WorkDoneProgressBegin {
                cancellable: Some(true),
                ..self.begin_msg
            },
            _kind: PhantomData,
        }
    }
}

impl<B, C> Progress<B, C> {
    /// Includes an optional more detailed progress message.
    ///
    /// This message is expected to contain information complementary to the `title` string passed
    /// into [`Client::progress`], such as `"3/25 files"`, `"project/src/module2"`, or
    /// `"node_modules/some_dep"`.
    pub fn with_message<M>(mut self, message: M) -> Self
    where
        M: Into<String>,
    {
        self.begin_msg.message = Some(message.into());
        self
    }

    /// Starts reporting progress to the client, returning an [`OngoingProgress`] handle.
    ///
    /// # Initialization
    ///
    /// This notification will only be sent if the server is initialized.
    pub async fn begin(self) -> OngoingProgress<B, C> {
        self.client
            .send_notification::<ProgressNotification>(ProgressParams {
                token: self.token.clone(),
                value: ProgressParamsValue::WorkDone(WorkDoneProgress::Begin(self.begin_msg)),
            })
            .await;

        OngoingProgress {
            client: self.client,
            token: self.token,
            _kind: PhantomData,
        }
    }
}

impl<B, C> Debug for Progress<B, C> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.debug_struct(stringify!(Progress))
            .field("token", &self.token)
            .field("properties", &self.begin_msg)
            .finish()
    }
}

/// An ongoing stream of progress being reported to the client.
///
/// This struct is created by [`Progress::begin`]. See its documentation for more.
#[must_use = "ongoing progress is not reported until `.report()` and/or `.finish()` is called"]
pub struct OngoingProgress<B, C> {
    client: Client,
    token: ProgressToken,
    _kind: PhantomData<(B, C)>,
}

impl<B, C> OngoingProgress<B, C> {
    async fn send_progress_report(&self, report: WorkDoneProgressReport) {
        self.client
            .send_notification::<ProgressNotification>(ProgressParams {
                token: self.token.clone(),
                value: ProgressParamsValue::WorkDone(WorkDoneProgress::Report(report)),
            })
            .await;
    }
}

impl OngoingProgress<Unbounded, NotCancellable> {
    /// Updates the secondary progress message visible in the client UI.
    ///
    /// This message is expected to contain information complementary to the `title` string passed
    /// into [`Client::progress`], such as `"3/25 files"`, `"project/src/module2"`, or
    /// `"node_modules/some_dep"`.
    ///
    /// # Initialization
    ///
    /// This notification will only be sent if the server is initialized.
    pub async fn report<M>(&self, message: M)
    where
        M: Into<String>,
    {
        self.send_progress_report(WorkDoneProgressReport {
            message: Some(message.into()),
            ..Default::default()
        })
        .await;
    }
}

impl OngoingProgress<Unbounded, Cancellable> {
    /// Enables or disables the "cancel" button in the client UI.
    ///
    /// # Initialization
    ///
    /// This notification will only be sent if the server is initialized.
    pub async fn report(&self, enable_cancel_btn: bool) {
        self.send_progress_report(WorkDoneProgressReport {
            cancellable: Some(enable_cancel_btn),
            ..Default::default()
        })
        .await;
    }

    /// Updates the secondary progress message visible in the client UI and optionally
    /// enables/disables the "cancel" button.
    ///
    /// This message is expected to contain information complementary to the `title` string passed
    /// into [`Client::progress`], such as `"3/25 files"`, `"project/src/module2"`, or
    /// `"node_modules/some_dep"`.
    ///
    /// If `enable_cancel_btn` is `None`, the state of the "cancel" button in the UI is unchanged.
    ///
    /// # Initialization
    ///
    /// This notification will only be sent if the server is initialized.
    pub async fn report_with_message<M>(&self, message: M, enable_cancel_btn: Option<bool>)
    where
        M: Into<String>,
    {
        self.send_progress_report(WorkDoneProgressReport {
            cancellable: enable_cancel_btn,
            message: Some(message.into()),
            ..Default::default()
        })
        .await;
    }
}

impl OngoingProgress<Bounded, NotCancellable> {
    /// Updates the progress percentage displayed in the client UI, where a value of `100` for
    /// example is considered 100% by the client.
    ///
    /// # Initialization
    ///
    /// This notification will only be sent if the server is initialized.
    pub async fn report(&self, percentage: u32) {
        self.send_progress_report(WorkDoneProgressReport {
            percentage: Some(percentage),
            ..Default::default()
        })
        .await;
    }

    /// Same as [`OngoingProgress::report`](OngoingProgress#method.report-2), except it also
    /// displays an optional more detailed progress message.
    ///
    /// This message is expected to contain information complementary to the `title` string passed
    /// into [`Client::progress`], such as `"3/25 files"`, `"project/src/module2"`, or
    /// `"node_modules/some_dep"`.
    ///
    /// # Initialization
    ///
    /// This notification will only be sent if the server is initialized.
    pub async fn report_with_message<M>(&self, message: M, percentage: u32)
    where
        M: Into<String>,
    {
        self.send_progress_report(WorkDoneProgressReport {
            message: Some(message.into()),
            percentage: Some(percentage),
            ..Default::default()
        })
        .await;
    }
}

impl OngoingProgress<Bounded, Cancellable> {
    /// Updates the progress percentage displayed in the client UI, where a value of `100` for
    /// example is considered 100% by the client.
    ///
    /// If `enable_cancel_btn` is `None`, the state of the "cancel" button in the UI is unchanged.
    ///
    /// # Initialization
    ///
    /// This notification will only be sent if the server is initialized.
    pub async fn report(&self, percentage: u32, enable_cancel_btn: Option<bool>) {
        self.send_progress_report(WorkDoneProgressReport {
            cancellable: enable_cancel_btn,
            message: None,
            percentage: Some(percentage),
        })
        .await;
    }

    /// Same as [`OngoingProgress::report`](OngoingProgress#method.report-3), except it also
    /// displays an optional more detailed progress message.
    ///
    /// This message is expected to contain information complementary to the `title` string passed
    /// into [`Client::progress`], such as `"3/25 files"`, `"project/src/module2"`, or
    /// `"node_modules/some_dep"`.
    ///
    /// # Initialization
    ///
    /// This notification will only be sent if the server is initialized.
    pub async fn report_with_message<M>(
        &self,
        message: M,
        percentage: u32,
        enable_cancel_btn: Option<bool>,
    ) where
        M: Into<String>,
    {
        self.send_progress_report(WorkDoneProgressReport {
            cancellable: enable_cancel_btn,
            message: Some(message.into()),
            percentage: Some(percentage),
        })
        .await;
    }
}

impl<C> OngoingProgress<Bounded, C> {
    /// Discards the progress bound associated with this `OngoingProgress`.
    ///
    /// All subsequent progress reports will no longer show a percentage value.
    pub fn into_unbounded(self) -> OngoingProgress<Unbounded, C> {
        OngoingProgress {
            client: self.client,
            token: self.token,
            _kind: PhantomData,
        }
    }
}

impl<B, C> OngoingProgress<B, C> {
    /// Indicates this long-running operation is complete.
    ///
    /// # Initialization
    ///
    /// This notification will only be sent if the server is initialized.
    pub async fn finish(self) {
        self.finish_inner(None).await;
    }

    /// Same as [`OngoingProgress::finish`], except it also displays an optional more detailed
    /// progress message.
    ///
    /// This message is expected to contain information complementary to the `title` string passed
    /// into [`Client::progress`], such as `"3/25 files"`, `"project/src/module2"`, or
    /// `"node_modules/some_dep"`.
    ///
    /// # Initialization
    ///
    /// This notification will only be sent if the server is initialized.
    pub async fn finish_with_message<M>(self, message: M)
    where
        M: Into<String>,
    {
        self.finish_inner(Some(message.into())).await;
    }

    async fn finish_inner(self, message: Option<String>) {
        self.client
            .send_notification::<ProgressNotification>(ProgressParams {
                token: self.token,
                value: ProgressParamsValue::WorkDone(WorkDoneProgress::End(
                    lsp_types::WorkDoneProgressEnd { message },
                )),
            })
            .await;
    }

    /// Returns the `ProgressToken` associated with this long-running operation.
    pub fn token(&self) -> &ProgressToken {
        &self.token
    }
}

impl<B, C> Debug for OngoingProgress<B, C> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.debug_struct(stringify!(OngoingProgress))
            .field("token", &self.token)
            .finish()
    }
}
