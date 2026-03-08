use std::os::fd::FromRawFd;
use std::os::unix::io::{AsRawFd, RawFd};
use std::{fs::OpenOptions, path::Path};
use tokio_rdma::DmaBuffer;

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

pub struct DMABuf {
    pub raw_fd: i32,
    pub offset: u64,
    pub size: usize,
}

impl AsRawFd for DMABuf {
    fn as_raw_fd(&self) -> RawFd {
        self.raw_fd
    }
}

impl DmaBuffer for DMABuf {
    fn dmabuf_offset(&self) -> u64 {
        self.offset
    }

    fn dmabuf_length(&self) -> usize {
        self.size
    }
}

impl DMABuf {
    pub fn new(path: impl AsRef<Path>, offset: u64, size: usize) -> anyhow::Result<Self> {
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
