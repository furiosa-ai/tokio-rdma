use clap::Parser;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio_rdma::{MemoryRegion, RdmaListener, RdmaStream};

mod common;
use crate::common::DMABuf;

#[derive(Parser, Debug)]
struct Args {
    #[arg(short, long, default_value = "0.0.0.0:8080")]
    addr: String,

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

        let mr = stream.register_dmabuf_mr(dmabuf, access as i32)?;
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

    let addr: SocketAddr = args.addr.parse()?;
    println!("Binding to {}...", addr);
    let listener = RdmaListener::bind(addr).await?;
    println!("Listening...");

    args.dmabuf_size = 1 << 30;

    // Pre-export dmabuf if needed
    let maybe_dmabuf = if let Some(path) = &args.dmabuf_dev {
        Some(DMABuf::new(&path, args.dmabuf_offset, args.dmabuf_size)?)
    } else {
        println!("Using Host Memory");
        None
    };

    loop {
        let stream = listener.accept().await?;
        println!("Accepted connection!");
        let mr = register_mr(&stream, &maybe_dmabuf)?;
        println!("registered mr {mr:?}");
        let len = mr.len();

        let futures = (0..64).map(|_| stream.recv(mr.clone(), 0, len as u32));
        let results = futures::future::join_all(futures).await;

        for result in results {
            let wc = result?;
            println!("Recv completed: {wc:?}");
        }

        // if let Some(dmabuf) = &maybe_dmabuf {
        //     dmabuf.offset;
        //     let dram = std::fs::OpenOptions::new().read(true).write(true).open("/dev/rngd/npu3ch0").unwrap();
        //     let dram_addr = 0xC0_0000_0000;
        //     let mut v = vec![0u8; wc.byte_len as usize];
        //     dram.read_at(&mut v, dram_addr).unwrap();

        //     // let mmap = dmabuf.mmap()?;
        //     // let msg =
        //     //     unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, wc.byte_len as usize) };
        //     // println!(
        //     //     "Message received in DMABUF: {:?}",
        //     //     String::from_utf8_lossy(msg)
        //     // );
        // } else {
        //     let msg =
        //         unsafe { std::slice::from_raw_parts(mr.addr() as *const u8, wc.byte_len as usize) };
        //     println!("Message: {:?}", String::from_utf8_lossy(msg));
        // }

        // let wc = stream.send(&mr, 0, size as u32).await?;
        // println!(
        // "Send message! status: {:?}, len: {}",
        // wc.status, wc.byte_len
        // );
    }
}
