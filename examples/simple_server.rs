use clap::Parser;
use std::net::SocketAddr;
use tokio_rdma::RdmaListener;

mod common;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = common::Args::parse();

    let addr: SocketAddr = args.addr.parse()?;
    println!("Binding to {}...", addr);
    let listener = RdmaListener::bind(addr).await?;
    println!("Listening...");

    loop {
        let stream = listener.accept().await?;
        println!("Accepted connection!");

        let mr = common::create_mr(&stream, &args)?;
        println!("registered mr {mr:?}");

        // Register MR for sending metadata to client
        let mut meta_data = vec![0u8; 16];
        meta_data[0..8].copy_from_slice(&mr.addr().to_le_bytes());
        meta_data[8..12].copy_from_slice(&mr.rkey().to_le_bytes());
        let meta_mr = stream.register_mr(
            meta_data,
            rdma_sys::ibv_access_flags::IBV_ACCESS_LOCAL_WRITE.0 as i32,
        )?;

        // Register MR for syncing with client
        let sync_mr = stream.register_mr(
            vec![0u8; 1],
            rdma_sys::ibv_access_flags::IBV_ACCESS_LOCAL_WRITE.0 as i32,
        )?;

        println!("Waiting for client ready signal...");
        // Wait for client to be ready to receive metadata
        stream.recv(sync_mr.clone(), 0, 1).await?;

        println!("Sending metadata to client...");
        // Send metadata
        stream.send(meta_mr.clone(), 0, 16).await?;

        println!("Waiting for client to finish writing...");
        // Wait for client to finish RDMA writes
        stream.recv(sync_mr.clone(), 0, 1).await?;

        println!("Write operations from client completed!");
        // let data = mr.data().unwrap();
        // println!("{data:?}");
    }
}
