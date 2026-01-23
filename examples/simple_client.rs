use clap::Parser;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio_rdma::{MemoryRegion, RdmaBuilder, RdmaStream};

mod common;
use crate::common::DMABuf;

#[derive(Parser, Debug)]
struct Args {
    #[arg(short, long, default_value = "127.0.0.1:8080")]
    addr: String,

    /// Local address to bind to
    #[arg(long)]
    bind_addr: Option<String>,

    /// Path to the dmabuf device (enables dmabuf mode)
    #[arg(long)]
    dmabuf_dev: Option<String>,

    #[arg(long, default_value_t = 268435456)]
    dmabuf_offset: u64,

    #[arg(long, default_value_t = 4096)]
    dmabuf_size: usize,
}

fn register_mr(
    stream: &RdmaStream,
    maybe_dmabuf: &Option<DMABuf>,
) -> anyhow::Result<Arc<MemoryRegion>> {
    let mr = if let Some(dmabuf) = &maybe_dmabuf {
        let access = rdma_sys::ibv_access_flags::IBV_ACCESS_LOCAL_WRITE.0
            | rdma_sys::ibv_access_flags::IBV_ACCESS_REMOTE_READ.0;
        // | rdma_sys::ibv_access_flags::IBV_ACCESS_REMOTE_WRITE.0;

        let mr = unsafe {
            stream.register_dmabuf_mr(0, dmabuf.size as usize, dmabuf.raw_fd, access as i32)?
        };
        mr
    } else {
        stream.register_mr(1024)?
    };

    Ok(mr)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let mut args = Args::parse();
    args.dmabuf_size = 1 << 30;

    let addr: SocketAddr = args.addr.parse()?;
    let mut builder = RdmaBuilder::new();
    if let Some(bind_addr) = &args.bind_addr {
        let local_addr: SocketAddr = bind_addr.parse()?;
        builder = builder.bind_src(local_addr);
        println!("Connecting to {} from {}...", addr, local_addr);
    } else {
        println!("Connecting to {}...", addr);
    }

    let stream = builder.connect(addr).await?;
    println!("Connected!");

    // Pre-export dmabuf if needed
    let maybe_dmabuf = if let Some(path) = &args.dmabuf_dev {
        Some(DMABuf::new(&path, args.dmabuf_offset, args.dmabuf_size)?)
    } else {
        println!("Using Host Memory");
        None
    };

    let mr = register_mr(&stream, &maybe_dmabuf)?;
    println!("MR Registered. {mr:?}");

    let len = mr.len();
    let now = std::time::Instant::now();

    let futures = (0..64).map(|_| stream.send(mr.clone(), 0, len as u32));
    let results = futures::future::join_all(futures).await;
    let mut total_bytes = 0u64;
    for result in results {
        let wc = result?;
        total_bytes += wc.wc.byte_len as u64;
        println!("Send completed: {wc:?}");
    }

    let elapsed = now.elapsed();
    let bw = total_bytes as f64 / elapsed.as_nanos() as f64;
    println!(
        "transfered {} bytes for {}ms {}GiB/s",
        total_bytes,
        elapsed.as_millis(),
        bw
    );
    Ok(())
}
