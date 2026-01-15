# tokio-rdma

`tokio-rdma` is a high-performance, asynchronous Rust library that provides safe and idiomatic wrappers around `rdma-sys` (libibverbs and librdmacm). It is designed to work seamlessly with the [Tokio](https://tokio.rs) runtime, enabling efficient RDMA operations in async Rust applications.

> **Warning**: This library is currently experimental and intended for research and development purposes.

## Features

*   **Async/Await Support**: Fully integrated with Tokio using `AsyncFd` for non-blocking event polling (CQ and CM events).
*   **Safe Abstractions**: wrappers for `ibv_context`, `ibv_pd`, `ibv_cq`, `ibv_qp`, and `ibv_mr`.
*   **RDMA Connection Manager (CM)**: Simplify connection establishment (Connect, Accept, Resolve Route/Addr) using `librdmacm`.
*   **Zero-Copy Memory Registration**:
    *   Host Memory
    *   **DMABUF**: Support for `ibv_reg_dmabuf_mr` to register GPU/NPU memory directly.
    *   **P2P Memory**: Support for registering generic `mmap`'d PCI BAR memory.
*   **Error Handling**: Typed errors using `thiserror`.

## Prerequisites

You need the RDMA userspace libraries installed on your system.

On Ubuntu/Debian:
```bash
sudo apt-get install libibverbs-dev librdmacm-dev
```

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
tokio-rdma = { path = "." } # Or git url once published
tokio = { version = "1", features = ["full"] }
```

## Usage

### 1. RDMA Connection Manager (Recommended)

This example shows how to establish a connection using the RDMA CM (Client side).

```rust
use std::sync::Arc;
use tokio_rdma::{CmEventChannel, CmId, Device, ProtectionDomain, CompletionQueue, QueuePair, MemoryRegion, QpInitAttr};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Create Event Channel and ID
    let channel = Arc::new(CmEventChannel::new()?);
    let id = CmId::new(channel.clone())?;

    // 2. Resolve Address and Route
    let addr = "127.0.0.1:8080".parse()?;
    id.resolve_addr(addr)?;
    let _ = channel.get_event().await?; // Expect ADDR_RESOLVED
    id.resolve_route()?;
    let _ = channel.get_event().await?; // Expect ROUTE_RESOLVED

    // 3. Create Resources (PD, CQ, QP)
    let verbs = id.context();
    let device = Arc::new(Device { raw: std::ptr::null_mut(), context: verbs });
    let pd = ProtectionDomain::new(device.clone())?;
    let cq = CompletionQueue::new(device.clone(), 10)?;
    
    let qp = QueuePair::new_cm(pd.clone(), id.id, QpInitAttr {
        send_cq: cq.clone(), 
        recv_cq: cq.clone()
    })?;

    // 4. Register Memory and Connect
    let mr = MemoryRegion::register(pd.clone(), 1024)?;
    id.connect()?;
    let _ = channel.get_event().await?; // Expect ESTABLISHED

    // 5. Post Send
    unsafe { qp.post_send(&mr, 0, 11, 1, true)?; }
    let wc = cq.poll().await?;
    
    Ok(())
}
```

### 2. Manual Connection (TCP OOB)

You can also exchange QP/LID information manually via TCP (Out-of-Band) and transition QP states (`INIT` -> `RTR` -> `RTS`) manually. See `examples/client.rs` and `examples/server.rs`.

## Examples

The `examples/` directory contains several complete examples:

*   **Basic Send/Recv**: `client.rs` / `server.rs` (Uses TCP for OOB exchange).
*   **RDMA CM**: `cm_client.rs` / `cm_server.rs` (Uses `librdmacm`).
*   **DMABUF (NPU/GPU)**: `dmabuf_rdma.rs` (Exports DMABUF from a device and registers it).
*   **P2P Memory**: `p2p_cm_client.rs` (Maps PCI BAR memory and registers it).

### Running Examples

**1. Basic Server/Client:**

```bash
# Terminal 1
cargo run --example server

# Terminal 2
cargo run --example client
```

**2. Connection Manager (CM):**

```bash
# Terminal 1
cargo run --example cm_server

# Terminal 2
cargo run --example cm_client
```

## License

MIT
