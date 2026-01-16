use std::net::SocketAddr;
use tokio_rdma::{MemoryRegion, RdmaListener};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let addr: SocketAddr = "0.0.0.0:8080".parse()?;
    println!("Binding to {}...", addr);
    let listener = RdmaListener::bind(addr).await?;
    println!("Listening...");

    loop {
        let stream = listener.accept().await?;
        println!("Accepted connection!");

        // Register Memory
        let mut mr = MemoryRegion::register(stream.pd.clone(), 1024)?;

        // Post recv
        unsafe {
            stream.qp.post_recv(&mr, 0, 1024, 100)?;
        }

        let wc = stream.cq.poll().await?;
        println!(
            "Received message! status: {:?}, len: {}",
            wc.status, wc.byte_len
        );

        let msg = unsafe {
            std::slice::from_raw_parts(mr.addr() as *const u8, wc.byte_len as usize)
        };
        println!("Message: {:?}", String::from_utf8_lossy(msg));
    }
}
