use clap::Parser;
use std::net::SocketAddr;
use std::os::fd::FromRawFd;
use std::os::unix::io::AsRawFd;
use std::sync::Arc;
use std::{fs::OpenOptions, path::Path};
use tokio_rdma::{MemoryRegion, RdmaBuilder, RdmaStream};

const NPU_BAR_IOCTL_MAGIC: u8 = b'N';
const NPU_BAR_EXPORT_DMABUF: i32 = 0x01;

#[repr(C, packed)]
struct NpuDmabufRegion {
    offset: u64,
    size: u64,
    fd: i32,
}

nix::ioctl_readwrite!(
    ioctl_npu_bar_export_dmabuf,
    NPU_BAR_IOCTL_MAGIC,
    NPU_BAR_EXPORT_DMABUF,
    NpuDmabufRegion
);

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

struct DMABuf {
    raw_fd: i32,
    offset: u64,
    size: usize,
}

impl DMABuf {
    fn new(path: impl AsRef<Path>, offset: u64, size: usize) -> anyhow::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;

        let mut region = NpuDmabufRegion {
            offset,
            size: size as u64,
            fd: -1,
        };

        unsafe { ioctl_npu_bar_export_dmabuf(file.as_raw_fd(), &mut region)? };

        let raw_fd = region.fd;
        println!("Exported dmabuf fd: {}", raw_fd);
        Ok(Self {
            raw_fd,
            offset,
            size,
        })
    }

    #[allow(dead_code)]
    fn mmap(&self) -> anyhow::Result<memmap2::MmapMut> {
        unsafe {
            let file = std::fs::File::from_raw_fd(self.raw_fd);
            Ok(memmap2::MmapOptions::new()
                .len(self.size as usize)
                .offset(self.offset)
                .map_mut(&file)?)
        }
    }
}

impl Drop for DMABuf {
    fn drop(&mut self) {
        if self.raw_fd >= 0 {
            unsafe { libc::close(self.raw_fd) };
        }
    }
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
