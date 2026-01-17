use clap::Parser;
use std::fs::OpenOptions;
use std::net::SocketAddr;
use std::os::unix::io::AsRawFd;
use tokio_rdma::{MemoryRegion, RdmaStream};

#[repr(C, packed)]
struct NpuDmabufRegion {
    offset: u64,
    size: u64,
    fd: i32,
}

const NPU_BAR_EXPORT_DMABUF: u64 = 0xc0144e01;

#[derive(Parser, Debug)]
struct Args {
    #[arg(short, long, default_value = "127.0.0.1:8080")]
    addr: String,

    /// Path to the dmabuf device (enables dmabuf mode)
    #[arg(long)]
    dmabuf_dev: Option<String>,

    #[arg(long, default_value_t = 268435456)]
    dmabuf_offset: u64,

    #[arg(long, default_value_t = 4096)]
    dmabuf_size: u64,
}

struct FileDescriptor(i32);

impl Drop for FileDescriptor {
    fn drop(&mut self) {
        if self.0 >= 0 {
            unsafe { libc::close(self.0) };
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let mut args = Args::parse();

    args.dmabuf_size = 1 << 30;

    let addr: SocketAddr = args.addr.parse()?;
    println!("Connecting to {}...", addr);
    let mut stream = RdmaStream::connect(addr).await?;
    println!("Connected!");

    // Helper to keep the fd alive if needed
    let _dmabuf_fd_guard;

    let mr = if let Some(path) = &args.dmabuf_dev {
        println!("Using DMABUF from {}", path);
        let file = OpenOptions::new().read(true).write(true).open(path)?;

        let mut region = NpuDmabufRegion {
            offset: args.dmabuf_offset,
            size: args.dmabuf_size,
            fd: -1,
        };

        let ret = unsafe {
            libc::ioctl(
                file.as_raw_fd(),
                NPU_BAR_EXPORT_DMABUF as libc::c_ulong,
                &mut region,
            )
        };
        if ret != 0 {
            anyhow::bail!("ioctl failed: {}", std::io::Error::last_os_error());
        }

        let fd = region.fd;
        println!("Exported dmabuf fd: {}", fd);

        let access = rdma_sys::ibv_access_flags::IBV_ACCESS_LOCAL_WRITE.0
            | rdma_sys::ibv_access_flags::IBV_ACCESS_REMOTE_READ.0;
        // | rdma_sys::ibv_access_flags::IBV_ACCESS_ZERO_BASED.0;
        // | rdma_sys::ibv_access_flags::IBV_ACCESS_REMOTE_WRITE.0;

        let mr = MemoryRegion::register_dmabuf(
            stream.pd.clone(),
            0,
            args.dmabuf_size as usize,
            fd,
            access as i32,
        )?;

        _dmabuf_fd_guard = Some(FileDescriptor(fd));
        mr
    } else {
        println!("Using Host Memory");
        _dmabuf_fd_guard = None;
        let mut mr = MemoryRegion::register(stream.pd.clone(), 1024)?;
        let msg = b"Hello High-Level RDMA!";
        unsafe {
            mr.as_mut_slice()[..msg.len()].copy_from_slice(msg);
        }
        mr
    };

    println!("MR Registered. LKey: {}", mr.lkey());

    let len = if args.dmabuf_dev.is_some() {
        args.dmabuf_size as u32
    } else {
        22 // "Hello High-Level RDMA!".len()
    };

    let now = std::time::Instant::now();

    for _ in 0..10 {
        let wc = stream.send(&mr, 0, len).await?;
        println!("Send completed: {} {:?}", wc.wr_id, wc.status);
    }

    let elapsed = now.elapsed();
    println!("for {}ms", elapsed.as_millis());

    let wc = stream.recv(&mr, 0, len).await?;

    let elapsed = now.elapsed();
    println!(
        "Recv completed: {} {:?} for {}ms",
        wc.wr_id,
        wc.status,
        elapsed.as_millis()
    );

    Ok(())
}
