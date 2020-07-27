pub mod app;
pub mod channel;
pub mod event;

#[derive(Debug)]
pub enum DataFormat {
    String,
    BinaryLE,
}
