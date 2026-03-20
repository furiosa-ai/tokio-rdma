use std::io;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RdmaError {
    #[error("IO Error: {0}")]
    Io(#[from] io::Error),
    #[error("RDMA Error: {0}")]
    Rdma(String),
    #[error("Device not found")]
    DeviceNotFound,
    #[error("Operation failed")]
    OperationFailed,
}

pub type Result<T> = std::result::Result<T, RdmaError>;
