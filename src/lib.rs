//! # tokio-rdma
//!
//! `tokio-rdma` is a high-performance, asynchronous Rust library that provides safe and idiomatic
//! wrappers around `rdma-sys` (based on `rdma-core`). It is designed to work seamlessly with the
//! [Tokio](https://tokio.rs) runtime, enabling efficient RDMA operations in async Rust applications.
//!
//! ## Features
//!
//! * **Async/Await Support**: Fully integrated with Tokio using `AsyncFd` for non-blocking event polling.
//! * **Safe Abstractions**: wrappers for `ibv_context`, `ibv_pd`, `ibv_cq`, `ibv_qp`, and `ibv_mr`.
//! * **RDMA Connection Manager (CM)**: Simplify connection establishment using `rdma-core`.
//! * **Zero-Copy Memory Registration**: Support for Host memory, DMABUF (GPU/NPU), and P2P memory.
//!
//! ## Example
//!
//! ```rust,no_run
//! # use tokio_rdma::RdmaBuilder;
//! # use std::net::SocketAddr;
//! # #[tokio::main]
//! # async fn main() -> anyhow::Result<()> {
//! #     let addr: SocketAddr = "127.0.0.1:8080".parse()?;
//! #     let stream = RdmaBuilder::new().connect(addr).await?;
//! #     let data = vec![77u8; 1024];
//! #     let mr = stream.register_mr(data, 1)?;
//! #     stream.send(mr.clone(), 0, 1024).await?;
//! #     Ok(())
//! # }
//! ```

pub mod cm;
pub mod cq;
pub mod device;
pub mod error;
pub mod mr;
pub mod pd;
pub mod qp;
pub mod stream;
pub mod utils;

pub use cm::{CmEvent, CmEventChannel, CmId};
pub use cq::CompletionQueue;
pub use device::{Device, DeviceList};
pub use error::RdmaError;
pub use mr::{DmaBuf, MemoryRegion};
pub use pd::ProtectionDomain;
pub use qp::{QpInitAttr, QueuePair};
pub use stream::{RdmaBuilder, RdmaListener, RdmaStream};
