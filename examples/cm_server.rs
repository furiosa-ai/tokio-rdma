use std::net::SocketAddr;
use std::sync::Arc;
use tokio_rdma::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let channel = Arc::new(CmEventChannel::new()?);
    let listen_id = CmId::new(channel.clone())?;

    let addr: SocketAddr = "0.0.0.0:8080".parse()?;
    listen_id.bind(addr)?;
    listen_id.listen(1)?;
    println!("CM Server listening on 8080...");

    // 1. Wait for connection request
    let event = channel.get_event().await?;
    if event.event_type() != rdma_sys::rdma_cm_event_type::RDMA_CM_EVENT_CONNECT_REQUEST {
        anyhow::bail!("Expected CONNECT_REQUEST, got {:?}", event.event_type());
    }

    let client_id_raw = event.id();
    // We need to associate verbs context and PD
    // The CM ID already has the verbs context after bind/listen/request
    let verbs = unsafe { (*client_id_raw).verbs };
    let device_raw = unsafe { (*verbs).device };
    let device = Arc::new(unsafe { Device::from_context(verbs, device_raw) });

    let pd = ProtectionDomain::new(device.clone())?;
    let cq = CompletionQueue::new(device.clone(), 10)?;

    // Create QP for the client ID
    let qp = QueuePair::new_cm(
        pd.clone(),
        client_id_raw,
        QpInitAttr {
            send_cq: cq.clone(),
            recv_cq: cq.clone(),
        },
    )?;

    // Register memory
    let mr = MemoryRegion::register(pd.clone(), 1024)?;

    // Post recv BEFORE accepting
    unsafe {
        qp.post_recv(&mr, 0, 1024, 100)?;
    }

    // 2. Accept connection
    let mut conn_param: rdma_sys::rdma_conn_param = unsafe { std::mem::zeroed() };
    conn_param.responder_resources = 1;
    conn_param.initiator_depth = 1;
    let ret = unsafe { rdma_sys::rdma_accept(client_id_raw, &mut conn_param) };
    if ret != 0 {
        anyhow::bail!("Failed to accept connection");
    }

    // 3. Wait for ESTABLISHED
    let event = channel.get_event().await?;
    if event.event_type() != rdma_sys::rdma_cm_event_type::RDMA_CM_EVENT_ESTABLISHED {
        anyhow::bail!("Expected ESTABLISHED, got {:?}", event.event_type());
    }

    println!("Connection established!");

    // 4. Wait for message
    println!("Waiting for message...");
    let wc = cq.poll().await?;
    println!(
        "Received message! status: {:?}, wr_id: {}",
        wc.status, wc.wr_id
    );

    let data = unsafe { mr.as_slice() };
    println!("Message: {}", String::from_utf8_lossy(&data[..12]));

    Ok(())
}

use std::ptr;
