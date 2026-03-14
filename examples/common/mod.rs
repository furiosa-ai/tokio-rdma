use clap::Parser;
use std::os::fd::FromRawFd;
use std::os::unix::io::AsRawFd;
use std::sync::Arc;
use std::{fs::OpenOptions, path::Path};
use tokio_rdma::error::Result;
use tokio_rdma::{DmaBuf, MemoryRegion, RdmaStream};

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

pub fn create_npu_dmabuf(
    path: impl AsRef<Path>,
    offset: u64,
    size: usize,
) -> anyhow::Result<DmaBuf> {
    let file = OpenOptions::new().read(true).write(true).open(path)?;

    const NPU_DEFAULT_OFFSET: u64 = 256 << 20;
    let mut region = NpuDmabufRegion {
        offset: offset + NPU_DEFAULT_OFFSET,
        size: size as u64,
        fd: -1,
    };

    unsafe { ioctl_npu_bar_export_dmabuf(file.as_raw_fd(), &mut region)? };

    let raw_fd = region.fd;
    println!("Exported dmabuf fd: {}", raw_fd);
    let dmabuf_file = unsafe { std::fs::File::from_raw_fd(raw_fd) };
    Ok(DmaBuf::new(dmabuf_file, size))
}

pub fn create_mr(stream: &RdmaStream, args: &Args) -> Result<Arc<MemoryRegion>> {
    let access = rdma_sys::ibv_access_flags::IBV_ACCESS_LOCAL_WRITE.0
        | rdma_sys::ibv_access_flags::IBV_ACCESS_REMOTE_READ.0
        | rdma_sys::ibv_access_flags::IBV_ACCESS_REMOTE_WRITE.0;

    // Pre-export dmabuf if needed
    if let Some(path) = &args.dmabuf_dev {
        let dmabuf = create_npu_dmabuf(path, args.dmabuf_offset, args.buffer_size).unwrap();
        stream.register_dmabuf_mr(dmabuf, 0, access as i32)
    } else {
        println!("Using Host Memory");
        let data = vec![77u8; args.buffer_size];
        stream.register_mr(data, access as i32)
    }
}

#[derive(Parser, Debug)]
pub struct Args {
    #[arg(short, long, default_value = "127.0.0.1:8080")]
    pub addr: String,

    /// Local address to bind to
    #[arg(long)]
    pub bind_addr: Option<String>,

    /// Path to the dmabuf device (enables dmabuf mode)
    #[arg(long)]
    pub dmabuf_dev: Option<String>,

    #[arg(long, default_value_t = 0)]
    pub dmabuf_offset: u64,

    #[arg(long, default_value_t = 4096)]
    pub buffer_size: usize,

    #[arg(long, default_value_t = 1)]
    pub count: usize,
}
