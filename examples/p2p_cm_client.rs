use clap::Parser;
use libc::{MAP_FAILED, MAP_SHARED, PROT_READ, PROT_WRITE, mmap, munmap};
use std::fs::OpenOptions;
use std::net::SocketAddr;
use std::os::unix::io::AsRawFd;
use std::sync::Arc;
use tokio_rdma::*;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to the PCI P2P device (e.g., /dev/nvme0n1 or /sys/class/pci_bus/.../p2pmem)
    #[arg(short, long)]
    path: String,

    /// IP address of the server
    #[arg(short, long, default_value = "127.0.0.1:8080")]
    addr: String,

    /// Size of the memory region to map
    #[arg(short, long, default_value_t = 4096)]
    size: usize,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    DeviceList::available().unwrap().print();

    // 1. Open and mmap the P2P device memory
    println!("Opening P2P device: {}", args.path);
    let file = OpenOptions::new().read(true).write(true).open(&args.path)?;

    let p2p_ptr = unsafe {
        mmap(
            std::ptr::null_mut(),
            args.size,
            PROT_READ | PROT_WRITE,
            MAP_SHARED,
            file.as_raw_fd(),
            0,
        )
    };

    if p2p_ptr == MAP_FAILED {
        return Err(anyhow::anyhow!(
            "mmap of P2P device failed. Check permissions and if device supports P2PMEM."
        ));
    }

    println!("P2P memory mmap'd at {:p}", p2p_ptr);

    // 2. Setup RDMA Connection Manager
    let channel = Arc::new(CmEventChannel::new()?);
    let id = CmId::new(channel.clone())?;

    let server_addr: SocketAddr = args.addr.parse()?;

    println!("Resolving address to {}...", server_addr);
    id.resolve_addr(server_addr)?;
    let event = channel.get_event().await?;
    if event.event_type() != rdma_sys::rdma_cm_event_type::RDMA_CM_EVENT_ADDR_RESOLVED {
        anyhow::bail!("Failed to resolve address: {:?}", event.event_type());
    }

    println!("Resolving route...");
    id.resolve_route()?;
    let event = channel.get_event().await?;
    if event.event_type() != rdma_sys::rdma_cm_event_type::RDMA_CM_EVENT_ROUTE_RESOLVED {
        anyhow::bail!("Failed to resolve route: {:?}", event.event_type());
    }

    // 3. Setup RDMA resources using the verbs context from CM ID
    let verbs = id.context();
    let device_raw = unsafe { (*verbs).device };

    let device = Arc::new(unsafe { Device::from_context(verbs, device_raw) });

    println!("Using RDMA device: {}", device.name());

    let pd = ProtectionDomain::new(device.clone())?;
    let cq = CompletionQueue::new(device.clone(), 10)?;

    // Create QP associated with the CM ID
    let qp = QueuePair::new_cm(
        pd.clone(),
        id.id,
        QpInitAttr {
            send_cq: cq.clone(),
            recv_cq: cq.clone(),
        },
    )?;

    // 4. Register the P2P memory region
    let access = rdma_sys::ibv_access_flags::IBV_ACCESS_LOCAL_WRITE.0
        | rdma_sys::ibv_access_flags::IBV_ACCESS_REMOTE_READ.0
        | rdma_sys::ibv_access_flags::IBV_ACCESS_REMOTE_WRITE.0;

    let mut mr =
        unsafe { MemoryRegion::register_user(pd.clone(), p2p_ptr, args.size, access as i32)? };

    println!(
        "P2P Memory registered. LKey: {}, RKey: {}",
        mr.lkey(),
        mr.rkey()
    );

    // 5. Connect
    println!("Connecting...");
    id.connect()?;
    let event = channel.get_event().await?;
    if event.event_type() != rdma_sys::rdma_cm_event_type::RDMA_CM_EVENT_ESTABLISHED {
        anyhow::bail!("Failed to establish connection: {:?}", event.event_type());
    }

    println!("RDMA Connection established!");

    // 6. Write data to P2P memory and Send
    let msg = b"P2P RDMA MSG";
    unsafe {
        let slice = mr.as_mut_slice();
        slice[..msg.len()].copy_from_slice(msg);
    }

    println!("Sending message from P2P memory...");
    unsafe {
        qp.post_send(&mr, 0, msg.len() as u32, 300, true)?;
    }

    let wc = cq.poll().await?;
    if wc.status == rdma_sys::ibv_wc_status::IBV_WC_SUCCESS {
        println!("Successfully sent data directly from PCI device memory!");
    } else {
        println!("Send failed with status: {:?}", wc.status);
    }

    // Cleanup
    unsafe { munmap(p2p_ptr, args.size) };

    Ok(())
}
