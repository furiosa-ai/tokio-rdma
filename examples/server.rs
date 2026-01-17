use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
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
    let my_psn = 0x1234;
    let my_gid = device.get_gid(port_num, 0)?;

    qp.to_init(port_num)?;

    let mr = MemoryRegion::register(pd.clone(), 1024)?;

    // Start TCP server for OOB
    let listener = TcpListener::bind("0.0.0.0:8080").await?;
    println!("Server listening on 8080 for OOB exchange...");

    let (mut socket, _) = listener.accept().await?;

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

    // Post a receive buffer
    unsafe {
        qp.post_recv(&mr, 0, 1024, 100)?;
    }

    println!("Waiting for message from client...");
    let wc = cq.poll().await?;
    println!(
        "Received message! WC status: {:?}, wr_id: {}",
        wc.status, wc.wr_id
    );

    let data = unsafe { mr.as_slice() };
    let msg = String::from_utf8_lossy(&data[..12]); // assume 12 bytes "Hello RDMA!"
    println!("Message content: {}", msg);

    Ok(())
}
