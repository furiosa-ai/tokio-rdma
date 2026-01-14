use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rdma::*;

#[derive(Serialize, Deserialize, Debug)]
struct ExchangeData {
    lid: u16,
    qpn: u32,
    psn: u32,
    rkey: u32,
    vaddr: u64,
    gid: [u8; 16],
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let device = Device::open(None)?;
    let pd = ProtectionDomain::new(device.clone())?;
    let cq = CompletionQueue::new(device.clone(), 10)?;

    let qp = QueuePair::new(
        pd.clone(),
        QpInitAttr {
            send_cq: cq.clone(),
            recv_cq: cq.clone(),
        },
    )?;

    let port_num = 1;
    let port_attr = device.get_port_attr(port_num)?;
    let my_lid = port_attr.lid;
    let my_qpn = qp.qp_num();
    let my_psn = 0x5678;
    let my_gid = device.get_gid(port_num, 0)?;

    qp.to_init(port_num)?;

    let mut mr = MemoryRegion::register(pd.clone(), 1024)?;

    // Connect to server via TCP for OOB
    println!("Connecting to server for OOB exchange...");
    let mut socket = TcpStream::connect("127.0.0.1:8080").await?;

    // Exchange data
    let my_data = ExchangeData {
        lid: my_lid,
        qpn: my_qpn,
        psn: my_psn,
        rkey: mr.rkey(),
        vaddr: mr.addr(),
        gid: unsafe { my_gid.raw },
    };

    let my_data_json = serde_json::to_vec(&my_data)?;
    socket.write_u32(my_data_json.len() as u32).await?;
    socket.write_all(&my_data_json).await?;

    let len = socket.read_u32().await?;
    let mut buf = vec![0u8; len as usize];
    socket.read_exact(&mut buf).await?;
    let peer_data: ExchangeData = serde_json::from_slice(&buf)?;

    println!("Received peer data: {:?}", peer_data);

    // Transition QP
    let dest_gid = if peer_data.lid == 0 {
        Some(peer_data.gid)
    } else {
        None
    };
    qp.to_rtr(
        peer_data.qpn,
        peer_data.lid,
        port_num,
        peer_data.psn,
        dest_gid,
    )?;
    qp.to_rts(my_psn)?;

    println!("QP transitioned to RTS");

    // Prepare message
    let msg = b"Hello RDMA!";
    unsafe {
        let slice = mr.as_mut_slice();
        slice[..msg.len()].copy_from_slice(msg);
    }

    // Send message
    println!("Sending message to server...");
    unsafe {
        qp.post_send(&mr, 0, msg.len() as u32, 200, true)?;
    }

    let wc = cq.poll().await?;
    println!(
        "Send completed! WC status: {:?}, wr_id: {}",
        wc.status, wc.wr_id
    );

    Ok(())
}
