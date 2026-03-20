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

    let meta_mr = stream.register_mr(
        vec![0u8; 16],
        rdma_sys::ibv_access_flags::IBV_ACCESS_LOCAL_WRITE.0 as i32,
    )?;
    let sync_mr = stream.register_mr(
        vec![1u8; 1],
        rdma_sys::ibv_access_flags::IBV_ACCESS_LOCAL_WRITE.0 as i32,
    )?;

    println!("Exchanging metadata with server...");

    // Post recv for metadata and send ready signal concurrently
    // so both are queued in the poller before waiting for completion.
    let (meta_res, sync_res) = tokio::join!(
        stream.recv(meta_mr.clone(), 0, 16),
        stream.send(sync_mr.clone(), 0, 1)
    );
    let _ = sync_res?;
    let meta_wc = meta_res?;
    println!("Metadata exchange completed: {meta_wc:?}");

    let meta_data = meta_mr.data().unwrap();
    let remote_addr = u64::from_le_bytes(meta_data[0..8].try_into().unwrap());
    let rkey = u32::from_le_bytes(meta_data[8..12].try_into().unwrap());
    println!("Server MR - addr: {:#x}, rkey: {:#x}", remote_addr, rkey);

    let len = mr.len();
    let now = std::time::Instant::now();

    println!("Performing {} RDMA writes...", args.count);
    let futures = (0..args.count).map(|_| async {
        let start = std::time::Instant::now();
        let res = stream
            .write(mr.clone(), 0, len as u32, remote_addr, rkey)
            .await;
        (res, start.elapsed())
    });

    let results = futures::future::join_all(futures).await;
    let mut total_bytes = 0u64;
    let mut total_latency = std::time::Duration::ZERO;
    let mut min_latency = std::time::Duration::MAX;
    let mut max_latency = std::time::Duration::ZERO;

    for (result, latency) in results {
        let wc = result?;
        total_bytes += len as u64;
        total_latency += latency;
        if latency < min_latency {
            min_latency = latency;
        }
        if latency > max_latency {
            max_latency = latency;
        }
        // println!("Write completed in {:?}: {:?}", latency, wc);
    }

    let elapsed = now.elapsed();
    let avg_latency = total_latency / args.count as u32;
    let bw = total_bytes as f64 / elapsed.as_nanos() as f64;
    println!(
        "transfered {} bytes for {}ms {}GiB/s",
        total_bytes,
        elapsed.as_millis(),
        bw
    );
    println!(
        "Latency - min: {:?}, max: {:?}, avg: {:?}",
        min_latency, max_latency, avg_latency
    );

    println!("Telling server we are done...");
    // Tell server we are done
    stream.send(sync_mr.clone(), 0, 1).await?;

    Ok(())
}
