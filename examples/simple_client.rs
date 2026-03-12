use clap::Parser;
use std::net::SocketAddr;
use tokio_rdma::RdmaBuilder;

mod common;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = common::Args::parse();

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

    let mr = common::create_mr(&stream, &args)?;
    println!("MR Registered. {mr:?}");

    let len = mr.len();
    let now = std::time::Instant::now();

    let futures = (0..args.count).map(|_| stream.send(mr.clone(), 0, len as u32));
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
