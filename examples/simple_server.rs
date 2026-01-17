use clap::Parser;
use std::net::SocketAddr;
use std::os::fd::FromRawFd;
use std::os::unix::io::AsRawFd;
use std::{fs::OpenOptions, path::Path};
use tokio_rdma::{MemoryRegion, RdmaListener};

#[repr(C, packed)]
struct NpuDmabufRegion {
    offset: u64,
    size: u64,
    fd: i32,
}

const NPU_BAR_EXPORT_DMABUF: u64 = 0xc0144e01;

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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let mut args = Args::parse();

    let addr: SocketAddr = args.addr.parse()?;
    println!("Binding to {}...", addr);
    let listener = RdmaListener::bind(addr).await?;
    println!("Listening...");

    args.dmabuf_size = 2 << 30;

    // Pre-export dmabuf if needed
    let maybe_dmabuf = if let Some(path) = &args.dmabuf_dev {
        Some(DMABuf::new(&path, args.dmabuf_offset, args.dmabuf_size)?)
    } else {
        println!("Using Host Memory");
        None
    };

    loop {
        let mut stream = listener.accept().await?;
        println!("Accepted connection!");

        let mr = if let Some(dmabuf) = &maybe_dmabuf {
            let access = rdma_sys::ibv_access_flags::IBV_ACCESS_LOCAL_WRITE.0
                | rdma_sys::ibv_access_flags::IBV_ACCESS_REMOTE_READ.0;
            // | rdma_sys::ibv_access_flags::IBV_ACCESS_REMOTE_WRITE.0;

            MemoryRegion::register_dmabuf(
                stream.pd.clone(),
                0,
                dmabuf.size as usize,
                dmabuf.raw_fd,
                access as i32,
            )?
        } else {
            MemoryRegion::register(stream.pd.clone(), 1024)?
        };

        let size = if let Some(_dmabuf) = &maybe_dmabuf {
            1 << 30
        } else {
            1024
        };
        // Post recv concurrently

        let futures = (0..10).map(|_| stream.recv(&mr, 0, size as u32));
        let results = futures::future::join_all(futures).await;

        for result in results {
            let wc = result?;
            println!("Recv completed: {} {:?}", wc.wr_id, wc.status);
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

        let wc = stream.send(&mr, 0, size as u32).await?;
        println!(
            "Send message! status: {:?}, len: {}",
            wc.status, wc.byte_len
        );
    }
}