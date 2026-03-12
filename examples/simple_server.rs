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

        let len = mr.len();

        let futures = (0..args.count).map(|_| stream.recv(mr.clone(), 0, len as u32));
        let results = futures::future::join_all(futures).await;

        for result in results {
            let wc = result?;
            println!("Recv completed: {wc:?}");
        }

        // let data = mr.data().unwrap();
        // println!("{data:?}");
    }
}
