use std::os::fd::FromRawFd;
use std::os::unix::io::AsRawFd;
use std::{fs::OpenOptions, path::Path};
use tokio_rdma::DmaBuf;

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

    let npu_default_offset = 256u64 << 20;
    let mut region = NpuDmabufRegion {
        offset: offset + npu_default_offset,
        size: size as u64,
        fd: -1,
    };

    unsafe { ioctl_npu_bar_export_dmabuf(file.as_raw_fd(), &mut region)? };

    let raw_fd = region.fd;
    println!("Exported dmabuf fd: {}", raw_fd);
    let dmabuf_file = unsafe { std::fs::File::from_raw_fd(raw_fd) };
    Ok(DmaBuf::new(dmabuf_file, size))
}
