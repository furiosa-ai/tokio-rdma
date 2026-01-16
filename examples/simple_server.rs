use clap::Parser;
use std::fs::OpenOptions;
use std::net::SocketAddr;
use std::os::unix::io::AsRawFd;
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
    let args = Args::parse();

    let addr: SocketAddr = args.addr.parse()?;
    println!("Binding to {}...", addr);
    let listener = RdmaListener::bind(addr).await?;
    println!("Listening...");

    // Pre-export dmabuf if needed
    let dmabuf_info = if let Some(path) = &args.dmabuf_dev {
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
        Some((FileDescriptor(fd), args.dmabuf_size))
    } else {
        println!("Using Host Memory");
        None
    };

    loop {
        let stream = listener.accept().await?;
        println!("Accepted connection!");

        let mut mr = if let Some((fd_wrapper, size)) = &dmabuf_info {
            let access = rdma_sys::ibv_access_flags::IBV_ACCESS_LOCAL_WRITE.0
		| rdma_sys::ibv_access_flags::IBV_ACCESS_REMOTE_READ.0; 
            // | rdma_sys::ibv_access_flags::IBV_ACCESS_REMOTE_WRITE.0;

            MemoryRegion::register_dmabuf(
                stream.pd.clone(),
                0,
                *size as usize,
                fd_wrapper.0,
                access as i32,
            )?
        } else {
            MemoryRegion::register(stream.pd.clone(), 1024)?
        };

        // Post recv
        unsafe {
            stream.qp.post_recv(&mr, 0, if dmabuf_info.is_some() { args.dmabuf_size as u32 } else { 1024 }, 100)?;
        }

        let wc = stream.cq.poll().await?;
        println!(
            "Received message! status: {:?}, len: {}",
            wc.status, wc.byte_len
        );

        if dmabuf_info.is_none() {
            let msg = unsafe {
                std::slice::from_raw_parts(mr.addr() as *const u8, wc.byte_len as usize)
            };
            println!("Message: {:?}", String::from_utf8_lossy(msg));
        } else {
            let (fd_wrapper, size) = dmabuf_info.as_ref().unwrap();
            let ptr = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    *size as usize,
                    libc::PROT_READ,
                    libc::MAP_SHARED,
                    fd_wrapper.0,
                    0,
                )
            };

            if ptr == libc::MAP_FAILED {
                eprintln!("mmap failed: {}", std::io::Error::last_os_error());
            } else {
                let msg = unsafe {
                    std::slice::from_raw_parts(ptr as *const u8, wc.byte_len as usize)
                };
                println!("Message received in DMABUF: {:?}", String::from_utf8_lossy(msg));

                unsafe { libc::munmap(ptr, *size as usize) };
            }
        }
    }
}
