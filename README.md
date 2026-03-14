# tokio-rdma

`tokio-rdma` is a high-performance, asynchronous Rust library that provides safe and idiomatic wrappers around `rdma-sys` (based on `rdma-core`). It is designed to work seamlessly with the [Tokio](https://tokio.rs) runtime, enabling efficient RDMA operations in async Rust applications.

> **Warning**: This library is currently experimental and intended for research and development purposes.

## Features

*   **Async/Await Support**: Fully integrated with Tokio using `AsyncFd` for non-blocking event polling (CQ and CM events).
*   **Safe Abstractions**: wrappers for `ibv_context`, `ibv_pd`, `ibv_cq`, `ibv_qp`, and `ibv_mr`.
*   **RDMA Connection Manager (CM)**: Simplify connection establishment (Connect, Accept, Resolve Route/Addr) using `rdma-core`.
*   **Zero-Copy Memory Registration**:
    *   Host Memory
    *   **DMABUF**: Support for `ibv_reg_dmabuf_mr` to register GPU/NPU memory directly.
    *   **P2P Memory**: Support for registering generic `mmap`'d PCI BAR memory.
*   **Error Handling**: Typed errors using `thiserror`.

## Prerequisites

You need the RDMA userspace libraries installed on your system.

On Ubuntu/Debian:
```bash
sudo apt-get install rdma-core libibverbs-dev librdmacm-dev
```

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
tokio-rdma = "0.1"
tokio = { version = "1", features = ["full"] }
```

## Usage

### 1. High-level Async Stream (Recommended)

This example shows how to establish a connection using the high-level `RdmaBuilder` and `RdmaListener`.

**Client:**
```rust
use tokio_rdma::RdmaBuilder;
use std::net::SocketAddr;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let addr: SocketAddr = "127.0.0.1:8080".parse()?;
    let stream = RdmaBuilder::new().connect(addr).await?;
    
    let data = vec![77u8; 1024];
    let mr = stream.register_mr(data, 1)?; // local write access
    
    stream.send(mr.clone(), 0, 1024).await?;
    Ok(())
}
```

**Server:**
```rust
use tokio_rdma::RdmaListener;
use std::net::SocketAddr;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let addr: SocketAddr = "127.0.0.1:8080".parse()?;
    let listener = RdmaListener::bind(addr).await?;
    
    let stream = listener.accept().await?;
    let data = vec![0u8; 1024];
    let mr = stream.register_mr(data, 1)?;
    
    stream.recv(mr.clone(), 0, 1024).await?;
    Ok(())
}
```

## Examples

The `examples/` directory contains complete examples for both client and server:

*   **Simple Client**: `simple_client.rs`
*   **Simple Server**: `simple_server.rs`

These examples support both Host memory and DMABUF (NPU/GPU) memory registration.

### Running Examples

**1. Basic Server/Client:**

```bash
# Terminal 1
cargo run --example simple_server

# Terminal 2
cargo run --example simple_client
```

## License

MIT
