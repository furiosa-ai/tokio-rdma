use std::net::SocketAddr;
use std::sync::Arc;
use tokio_rdma::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let channel = Arc::new(CmEventChannel::new()?);
    let id = CmId::new(channel.clone())?;

    // let addr: SocketAddr = "127.0.0.1:8080".parse()?;
    let addr: SocketAddr = "10.3.0.46:8080".parse()?;

    // 1. Resolve address
    id.resolve_addr(addr)?;
    let event = channel.get_event().await?;
    if event.event_type() != rdma_sys::rdma_cm_event_type::RDMA_CM_EVENT_ADDR_RESOLVED {
        anyhow::bail!("Expected ADDR_RESOLVED, got {:?}", event.event_type());
    }

    // 2. Resolve route
    id.resolve_route()?;
    let event = channel.get_event().await?;
    if event.event_type() != rdma_sys::rdma_cm_event_type::RDMA_CM_EVENT_ROUTE_RESOLVED {
        anyhow::bail!("Expected ROUTE_RESOLVED, got {:?}", event.event_type());
    }

    // Setup resources
    let verbs = id.context();
    let device_raw = unsafe { (*verbs).device };
    let device = Arc::new(unsafe { Device::from_context(verbs, device_raw) });
    let pd = ProtectionDomain::new(device.clone())?;
    let cq = CompletionQueue::new(device.clone(), 10)?;

    let qp = QueuePair::new_cm(
        pd.clone(),
        id.id,
        QpInitAttr {
            send_cq: cq.clone(),
            recv_cq: cq.clone(),
        },
    )?;

    let mut mr = MemoryRegion::register(pd.clone(), 1024)?;
    let msg = b"Hello RDMA!";
    unsafe {
        mr.as_mut_slice()[..msg.len()].copy_from_slice(msg);
    }

    // 3. Connect
    id.connect()?;
    let event = channel.get_event().await?;
    if event.event_type() != rdma_sys::rdma_cm_event_type::RDMA_CM_EVENT_ESTABLISHED {
        anyhow::bail!("Expected ESTABLISHED, got {:?}", event.event_type());
    }

    println!("Connection established!");

    // 4. Send message
    unsafe {
        qp.post_send(&mr, 0, msg.len() as u32, 200, true)?;
    }

    let wc = cq.poll().await?;
    println!(
        "Send completed! status: {:?}, wr_id: {}",
        wc.status, wc.wr_id
    );

    Ok(())
}
