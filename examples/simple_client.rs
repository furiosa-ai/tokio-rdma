use clap::Parser;
use std::net::SocketAddr;
use tokio_rdma::RdmaBuilder;

mod common;

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

    #[arg(long, default_value_t = 0)]
    dmabuf_offset: u64,

    #[arg(long, default_value_t = 4096)]
    buffer_size: usize,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

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

    let access = rdma_sys::ibv_access_flags::IBV_ACCESS_LOCAL_WRITE.0
        | rdma_sys::ibv_access_flags::IBV_ACCESS_REMOTE_READ.0
        | rdma_sys::ibv_access_flags::IBV_ACCESS_REMOTE_WRITE.0;

    // Pre-export dmabuf if needed
    let mr = if let Some(path) = &args.dmabuf_dev {
        let dmabuf =
            common::create_npu_dmabuf(&path, args.dmabuf_offset, args.buffer_size).unwrap();
        stream.register_dmabuf_mr(dmabuf, 0, access as i32).unwrap()
    } else {
        println!("Using Host Memory");
        let data = vec![77u8; args.buffer_size];
        stream.register_mr(data, access as i32).unwrap()
    };

    println!("MR Registered. {mr:?}");

    let len = mr.len();
    let now = std::time::Instant::now();

    let futures = (0..1).map(|_| stream.send(mr.clone(), 0, len as u32));
    let results = futures::future::join_all(futures).await;
    let mut total_bytes = 0u64;
    for result in results {
        let wc = result?;
        total_bytes += len as u64;
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
