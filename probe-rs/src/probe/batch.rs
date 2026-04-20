use std::cell::OnceCell;
use std::rc::Rc;

/// A batch of ordered transactions.
///
/// Each all the transactions are guaranteed to be executed in order.
pub(crate) struct Batch {
    transactions: Vec<Rc<Transaction>>,
}

impl Batch {
    /// Creates a new empty batch.
    pub(crate) fn new() -> Self {
        Self {
            transactions: Vec::new(),
        }
    }

    /// Add a new transaction to the batch.
    ///
    /// Returns the transaction handle.
    pub(crate) fn add_transaction(&mut self, transaction: impl Into<Transaction>) -> Response {
        let transaction = Rc::new(transaction.into());
        self.transactions.push(transaction.clone());
        Response(transaction)
    }

    /// Iterate over all the transactions in the batch.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &Transaction> {
        self.transactions.iter().map(|t| t.as_ref())
    }
}

impl Default for Batch {
    fn default() -> Self {
        Self::new()
    }
}

/// A transaction is a serialized request and a slot for its serialized response.
///
/// NOTE: The response is stored as raw bytes rather than a parsed value.
/// This is intentional so different probes can return variable-size responses depending
/// on the protocol.
/// Only the probe driver knows how to slice the response stream back into per-transaction chunks.
/// Deserialization is deferred to the handle's get() call.
pub(crate) struct Transaction {
    /// The request to be made for this transaction.
    request: Vec<u8>,

    /// The expected response length.
    ///
    /// This is required for the batch executor to know how to slice transaction responses.
    response_len: usize,

    /// The response to the request of this transaction.
    response: OnceCell<Vec<u8>>,
}

impl Transaction {
    /// Creates a new transaction from request bytes and the expected response length.
    pub(crate) fn new(request: Vec<u8>, response_len: usize) -> Self {
        Self {
            request,
            response_len,
            response: OnceCell::new(),
        }
    }

    /// Access the request bytes for this transaction.
    pub(crate) fn request(&self) -> &[u8] {
        &self.request
    }

    /// The expected response length for this transaction
    pub(crate) fn response_len(&self) -> usize {
        self.response_len
    }

    /// Set the response that belongs to this transaction.
    pub(crate) fn set_response(&self, response: Vec<u8>) {
        self.response
            .set(response)
            .expect("we cannot set a transaction result twice");
    }
}

/// A deferred result handle for a single transaction.
///
/// The value becomes available once the batch of the underlying transaction
/// has been executed.
pub(crate) trait Handle<'a> {
    /// The return type of the handle.
    type T;

    /// Get the result of the transaction.
    fn get(&'a self) -> Self::T {
        self.get_at(0)
    }

    /// Access a specific index in the transaction.
    ///
    /// This can be used for repeated transactions.
    fn get_at(&'a self, index: usize) -> Self::T;

    /// Takes a closure and creates a handle which calls that closure on the transaction result.
    fn map<B, F: Fn(Self::T) -> B>(self, f: F) -> MappedHandle<F, Self>
    where
        Self: Sized,
    {
        MappedHandle {
            inner: self,
            func: f,
        }
    }
}

/// Wraps another [`BatchResult`] and applies a transform function to its value.
/// This allows callers to compose handles without extra heap allocation.
pub(crate) struct MappedHandle<F, R> {
    /// The handle that is underlying this map.
    inner: R,
    /// The function to apply to the result of the underlying handle.
    func: F,
}

impl<'a, A, B, F, R> Handle<'a> for MappedHandle<F, R>
where
    R: Handle<'a, T = A>,
    F: Fn(A) -> B,
{
    type T = B;

    fn get_at(&'a self, index: usize) -> B {
        let r = self.inner.get_at(index);
        (self.func)(r)
    }
}

/// The response handle for a transaction.
pub(crate) struct Response(Rc<Transaction>);

impl<'a> Handle<'a> for Response {
    type T = &'a [u8];

    fn get_at(&'a self, _index: usize) -> &'a [u8] {
        &self.0.response.get().expect("execute batch first")[..]
    }
}
