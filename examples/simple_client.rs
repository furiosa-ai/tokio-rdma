use std::net::SocketAddr;
use tokio_rdma::{MemoryRegion, RdmaStream};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    // Connect
    let addr: SocketAddr = "127.0.0.1:8080".parse()?;
    println!("Connecting to {}...", addr);
    let stream = RdmaStream::connect(addr).await?;
    println!("Connected!");

    // Register Memory
    let mut mr = MemoryRegion::register(stream.pd.clone(), 1024)?;
    let msg = b"Hello High-Level RDMA!";
    unsafe {
        mr.as_mut_slice()[..msg.len()].copy_from_slice(msg);
    }

    // Send
    unsafe {
        stream.qp.post_send(&mr, 0, msg.len() as u32, 123, true)?;
    }

    let wc = stream.cq.poll().await?;
    println!("Send completed: {:?}", wc.status);

    Ok(())
}
