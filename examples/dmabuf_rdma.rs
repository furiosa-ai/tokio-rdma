use clap::Parser;
use std::fs::OpenOptions;
use std::net::SocketAddr;
use std::os::unix::io::AsRawFd;
use tokio_rdma::*;

#[repr(C, packed)]
struct NpuDmabufRegion {
    offset: u64,
    size: u64,
    fd: i32,
}

// Calculated for Linux x86_64: _IOWR('N', 0x01, struct npu_dmabuf_region)
const NPU_BAR_EXPORT_DMABUF: u64 = 0xc0144e01;

#[derive(Parser, Debug)]
struct Args {
    /// Path to the NPU bar device (e.g., /dev/rngd/npu0bar4)
    #[arg(short, long, default_value = "/dev/rngd/npu0bar4")]
    path: String,

    /// IP address of the server
    #[arg(short, long, default_value = "10.3.0.46:8080")]
    addr: String,

    /// Offset within the BAR (Must be >= 256MB for BAR4)
    #[arg(short, long, default_value_t = 268435456)]
    offset: u64,

    /// Size of the dmabuf to export
    #[arg(short, long, default_value_t = 4096)]
    size: u64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    println!("Args: offset=0x{:x}, size=0x{:x}", args.offset, args.size);

    // 1. Open the NPU bar device and export dmabuf
    println!("Opening NPU bar device: {}", args.path);
    let file = OpenOptions::new().read(true).write(true).open(&args.path)?;

    let mut region = NpuDmabufRegion {
        offset: args.offset,
        size: args.size,
        fd: -1,
    };

    let offset = region.offset;
    let size = region.size;
    println!(
        "Exporting dmabuf via ioctl (offset=0x{:x}, size=0x{:x})...",
        offset, size
    );
    let ret = unsafe {
        libc::ioctl(
            file.as_raw_fd(),
            NPU_BAR_EXPORT_DMABUF as libc::c_ulong,
            &mut region,
        )
    };

    if ret != 0 {
        return Err(anyhow::anyhow!(
            "ioctl NPU_BAR_EXPORT_DMABUF failed: {}. Make sure the driver is loaded and supports dmabuf export.",
            std::io::Error::last_os_error()
        ));
    }

    let fd = region.fd;
    let size = region.size;
    println!(
        "Successfully exported dmabuf! fd: {}, size: {} bytes",
        fd, size
    );

    // 2. Connect to server
    let server_addr: SocketAddr = args.addr.parse()?;
    println!("Connecting to {}...", server_addr);
    let stream = RdmaStream::connect(server_addr).await?;
    println!("RDMA Connection established!");

    // 3. Register the DMABUF Memory Region
    let access = rdma_sys::ibv_access_flags::IBV_ACCESS_LOCAL_WRITE.0
        | rdma_sys::ibv_access_flags::IBV_ACCESS_REMOTE_READ.0
        | rdma_sys::ibv_access_flags::IBV_ACCESS_REMOTE_WRITE.0;

    println!("Registering dmabuf with RDMA...");
    let mr = MemoryRegion::register_dmabuf(
        stream.pd.clone(),
        0, // Offset within the dmabuf
        size as usize,
        fd,
        access as i32,
    )?;

    println!(
        "DMABUF Memory Region registered. LKey: {}, RKey: {}",
        mr.lkey(),
        mr.rkey()
    );

    // Clean up dmabuf fd
    unsafe { libc::close(fd) };

    Ok(())
}
