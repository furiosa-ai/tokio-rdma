use clap::Parser;
use std::net::SocketAddr;
use tokio_rdma::RdmaListener;

mod common;

#[derive(Parser, Debug)]
struct Args {
    #[arg(short, long, default_value = "0.0.0.0:8080")]
    addr: String,

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
    println!("Binding to {}...", addr);
    let listener = RdmaListener::bind(addr).await?;
    println!("Listening...");

    loop {
        let stream = listener.accept().await?;
        println!("Accepted connection!");

        let access = rdma_sys::ibv_access_flags::IBV_ACCESS_LOCAL_WRITE.0
            | rdma_sys::ibv_access_flags::IBV_ACCESS_REMOTE_READ.0
            | rdma_sys::ibv_access_flags::IBV_ACCESS_REMOTE_WRITE.0;

        let mr = if let Some(path) = &args.dmabuf_dev {
            let dmabuf =
                common::create_npu_dmabuf(&path, args.dmabuf_offset, args.buffer_size).unwrap();
            stream.register_dmabuf_mr(dmabuf, 0, access as i32).unwrap()
        } else {
            println!("Using Host Memory");
            let data = vec![0u8; args.buffer_size];
            stream.register_mr(data, access as i32).unwrap()
        };
        println!("registered mr {mr:?}");

        let len = mr.len();

        let futures = (0..1).map(|_| stream.recv(mr.clone(), 0, len as u32));
        let results = futures::future::join_all(futures).await;

        for result in results {
            let wc = result?;
            println!("Recv completed: {wc:?}");
        }

        let data = mr.data().unwrap();
        println!("{data:?}");
    }
}
