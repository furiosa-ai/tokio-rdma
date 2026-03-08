use crate::MemoryRegion;
use crate::cm::{CmEventChannel, CmId};
use crate::cq::CompletionQueue;
use crate::device::Device;
use crate::error::{RdmaError, Result};
use crate::pd::ProtectionDomain;
use crate::qp::{QpInitAttr, QueuePair};
use rdma_sys::{ibv_wc, rdma_cm_event_type};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

#[derive(Debug)]
pub struct WorkCompletion {
    pub wc: rdma_sys::ibv_wc,
}

pub enum Request {
    Send(
        oneshot::Sender<Result<ibv_wc>>,
        Vec<(Arc<MemoryRegion>, u64, u32)>,
    ),
    Recv(
        oneshot::Sender<Result<ibv_wc>>,
        Vec<(Arc<MemoryRegion>, u64, u32)>,
    ),
    Read(
        oneshot::Sender<Result<ibv_wc>>,
        Vec<(Arc<MemoryRegion>, u64, u32)>,
        u64, // remote_addr
        u32, // rkey
    ),
    Write(
        oneshot::Sender<Result<ibv_wc>>,
        Vec<(Arc<MemoryRegion>, u64, u32)>,
        u64, // remote_addr
        u32, // rkey
    ),
}

pub struct RdmaStream {
    pub id: CmId,
    pub qp: Arc<QueuePair>,
    pub pd: Arc<ProtectionDomain>,
    pub cq: Arc<CompletionQueue>,
    poller_handle: JoinHandle<()>,
    tx: tokio::sync::mpsc::Sender<Request>,
}

impl Drop for RdmaStream {
    fn drop(&mut self) {
        self.poller_handle.abort();
    }
}

#[derive(Default)]
pub struct RdmaBuilder {
    src_addr: Option<SocketAddr>,
}

impl RdmaBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bind_src(mut self, addr: SocketAddr) -> Self {
        self.src_addr = Some(addr);
        self
    }

    pub async fn connect(self, addr: SocketAddr) -> Result<RdmaStream> {
        RdmaStream::connect_internal(self.src_addr, addr).await
    }
}

impl RdmaStream {
    pub fn builder() -> RdmaBuilder {
        RdmaBuilder::new()
    }

    pub(crate) async fn connect_internal(
        src_addr: Option<SocketAddr>,
        dst_addr: SocketAddr,
    ) -> Result<Self> {
        tracing::debug!("Connecting to {}", dst_addr);

        // 1. Setup Channel & ID
        let channel = Arc::new(CmEventChannel::new()?);
        let id = CmId::new(channel.clone())?;

        // 2. Resolve Address
        tracing::debug!("Resolving address...");
        id.resolve_addr(src_addr, dst_addr)?;
        let event = channel.get_event().await?;
        if event.event_type() != rdma_cm_event_type::RDMA_CM_EVENT_ADDR_RESOLVED {
            return Err(RdmaError::Rdma(format!(
                "Addr resolution failed: {:?}",
                event.event_type()
            )));
        }
        tracing::debug!("Address resolved");

        // 3. Resolve Route
        tracing::debug!("Resolving route...");
        id.resolve_route()?;
        let event = channel.get_event().await?;
        if event.event_type() != rdma_cm_event_type::RDMA_CM_EVENT_ROUTE_RESOLVED {
            return Err(RdmaError::Rdma(format!(
                "Route resolution failed: {:?}",
                event.event_type()
            )));
        }
        tracing::debug!("Route resolved");

        // 4. Create Resources
        let verbs = id.context();
        let device_raw = unsafe { (*verbs).device };
        let device = Arc::new(unsafe { Device::from_context(verbs, device_raw) });

        let pd = ProtectionDomain::new(device.clone())?;
        // Default CQ size 16 for now
        let cq = CompletionQueue::new(device.clone(), 16)?;

        let qp = QueuePair::new_cm(
            pd.clone(),
            id.id,
            QpInitAttr {
                send_cq: cq.clone(),
                recv_cq: cq.clone(),
            },
        )?;

        // 5. Connect
        tracing::debug!("Establishing connection...");
        id.connect()?;
        let event = channel.get_event().await?;
        if event.event_type() != rdma_cm_event_type::RDMA_CM_EVENT_ESTABLISHED {
            return Err(RdmaError::Rdma(format!(
                "Connection failed: {:?}",
                event.event_type()
            )));
        }
        tracing::debug!("Connection established");
        let (tx, rx) = tokio::sync::mpsc::channel(128);
        let poller_handle = Self::spawn_cq_poller(cq.clone(), rx, qp.clone());
        Ok(Self {
            id,
            qp,
            pd,
            cq,
            poller_handle,
            tx,
        })
    }

    pub async fn connect(addr: SocketAddr) -> Result<Self> {
        Self::connect_internal(None, addr).await
    }

    fn handle_completion(works: &mut HashMap<u64, oneshot::Sender<Result<ibv_wc>>>, wc: ibv_wc) {
        if let Some(tx) = works.remove(&wc.wr_id) {
            // Send the completion to the waiting task
            let _ = tx.send(Ok(wc));
        } else {
            // This might happen if the sender was dropped or cancelled
            // or if we have a spurious completion.
            // For now, we just ignore it.
            tracing::warn!("no wc entry for {wc:?}");
        }
    }

    fn handle_request(
        req: Request,
        works: &mut HashMap<u64, oneshot::Sender<Result<ibv_wc>>>,
        qp: Arc<QueuePair>,
        wr_id: &mut u64,
    ) {
        *wr_id = *wr_id + 1;
        match req {
            Request::Send(tx, requests) => {
                let reqs: Vec<_> = requests
                    .iter()
                    .map(|(mr, offset, len)| (mr.as_ref(), *offset, *len))
                    .collect();

                if let Err(e) = unsafe { qp.post_send_multi(reqs, *wr_id, true) } {
                    tx.send(Err(e)).unwrap();
                } else {
                    works.insert(*wr_id, tx);
                }
            }
            Request::Recv(tx, requests) => {
                let reqs: Vec<_> = requests
                    .iter()
                    .map(|(mr, offset, len)| (mr.as_ref(), *offset, *len))
                    .collect();

                if let Err(e) = unsafe { qp.post_recv_multi(reqs, *wr_id) } {
                    tx.send(Err(e)).unwrap();
                } else {
                    works.insert(*wr_id, tx);
                }
            }
            Request::Read(tx, requests, remote_addr, rkey) => {
                let reqs: Vec<_> = requests
                    .iter()
                    .map(|(mr, offset, len)| (mr.as_ref(), *offset, *len))
                    .collect();

                if let Err(e) = unsafe {
                    qp.post_rdma(
                        reqs,
                        rdma_sys::ibv_wr_opcode::IBV_WR_RDMA_READ,
                        *wr_id,
                        true,
                        remote_addr,
                        rkey,
                    )
                } {
                    tx.send(Err(e)).unwrap();
                } else {
                    works.insert(*wr_id, tx);
                }
            }
            Request::Write(tx, requests, remote_addr, rkey) => {
                let reqs: Vec<_> = requests
                    .iter()
                    .map(|(mr, offset, len)| (mr.as_ref(), *offset, *len))
                    .collect();

                if let Err(e) = unsafe {
                    qp.post_rdma(
                        reqs,
                        rdma_sys::ibv_wr_opcode::IBV_WR_RDMA_WRITE,
                        *wr_id,
                        true,
                        remote_addr,
                        rkey,
                    )
                } {
                    tx.send(Err(e)).unwrap();
                } else {
                    works.insert(*wr_id, tx);
                }
            }
        }
    }

    /// Helper to spawn the background polling task
    fn spawn_cq_poller(
        cq: Arc<CompletionQueue>,
        mut request_receiver: tokio::sync::mpsc::Receiver<Request>,
        qp: Arc<QueuePair>,
    ) -> JoinHandle<()> {
        tokio::task::spawn(async move {
            let mut wr_id = 0u64;
            let mut works = HashMap::new();
            loop {
                tokio::select! {
                    maybe_request = request_receiver.recv() => {
                        match maybe_request {
                            Some(req) => Self::handle_request(req, &mut works, qp.clone(), &mut wr_id),
                            None => break,
                        }
                    },
                    maybe_wc = cq.poll() =>
                    {
                        match maybe_wc {
                            Ok(wc) => Self::handle_completion(&mut works, wc),
                            Err(_e) => {
                                // Polling failed, likely CQ destroyed or device error
                                break;
                            }
                        }
                    }
                }
            }
        })
    }

    pub async fn send_multi(
        &self,
        requests: Vec<(Arc<MemoryRegion>, u64, u32)>,
    ) -> Result<WorkCompletion> {
        let (tx, rx) = oneshot::channel();
        self.tx.send(Request::Send(tx, requests)).await?;
        let wc = rx.await??;
        Ok(WorkCompletion { wc })
    }

    pub async fn recv_multi(
        &self,
        requests: Vec<(Arc<MemoryRegion>, u64, u32)>,
    ) -> Result<WorkCompletion> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Request::Recv(tx, requests))
            .await
            .map_err(|_| RdmaError::Rdma("Stream poller task is closed".to_string()))?;
        let wc = rx.await??;
        Ok(WorkCompletion { wc })
    }

    pub async fn send(
        &self,
        mr: Arc<MemoryRegion>,
        offset: u64,
        len: u32,
    ) -> Result<WorkCompletion> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Request::Send(tx, vec![(mr, offset, len)]))
            .await
            .map_err(|_| RdmaError::Rdma("Stream poller task is closed".to_string()))?;
        let wc = rx.await??;
        Ok(WorkCompletion { wc })
    }

    pub async fn recv(
        &self,
        mr: Arc<MemoryRegion>,
        offset: u64,
        len: u32,
    ) -> Result<WorkCompletion> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Request::Recv(tx, vec![(mr, offset, len)]))
            .await
            .map_err(|_| RdmaError::Rdma("Stream poller task is closed".to_string()))?;
        let wc = rx.await??;
        Ok(WorkCompletion { wc })
    }

    pub async fn read(
        &self,
        mr: Arc<MemoryRegion>,
        offset: u64,
        len: u32,
        remote_addr: u64,
        rkey: u32,
    ) -> Result<WorkCompletion> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Request::Read(
                tx,
                vec![(mr, offset, len)],
                remote_addr,
                rkey,
            ))
            .await?;
        let wc = rx.await??;
        Ok(WorkCompletion { wc })
    }

    pub async fn write(
        &self,
        mr: Arc<MemoryRegion>,
        offset: u64,
        len: u32,
        remote_addr: u64,
        rkey: u32,
    ) -> Result<WorkCompletion> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Request::Write(
                tx,
                vec![(mr, offset, len)],
                remote_addr,
                rkey,
            ))
            .await
            .map_err(|_| RdmaError::Rdma("Stream poller task is closed".to_string()))?;
        let wc = rx.await??;
        Ok(WorkCompletion { wc })
    }

    pub fn register_mr(&self, len: usize) -> Result<Arc<MemoryRegion>> {
        MemoryRegion::register(self.pd.clone(), len)
    }

    pub fn register_dmabuf_mr(
        &self,
        dmabuf: &impl crate::mr::DmaBuffer,
        access: i32,
    ) -> Result<Arc<MemoryRegion>> {
        MemoryRegion::register_dmabuf(self.pd.clone(), dmabuf, access)
    }
}

pub struct RdmaListener {
    pub listener_id: CmId,
    pub channel: Arc<CmEventChannel>,
}

impl RdmaListener {
    pub async fn bind(addr: SocketAddr) -> Result<Self> {
        tracing::debug!("Binding to {}", addr);
        let channel = Arc::new(CmEventChannel::new()?);
        let listener_id = CmId::new(channel.clone())?;
        listener_id.bind(addr)?;
        listener_id.listen(10)?;
        tracing::debug!("Listening on {}", addr);
        Ok(Self {
            listener_id,
            channel,
        })
    }

    pub async fn accept(&self) -> Result<RdmaStream> {
        tracing::debug!("Waiting for connection request...");
        loop {
            let event = self.channel.get_event().await?;
            tracing::debug!("Server received CM event: {}", event.event_type());
            if event.event_type() == rdma_cm_event_type::RDMA_CM_EVENT_CONNECT_REQUEST {
                tracing::debug!("Received connection request from id {:?}", event.id());
                let client_id_raw = event.id();

                // Wrap the ID initially with the listener's channel
                let mut client_id =
                    unsafe { CmId::from_raw(client_id_raw, Some(self.channel.clone())) };

                // Acknowledge the event NOW so the library can proceed
                drop(event);

                // Create a NEW channel for this connection to isolate it
                let new_channel = Arc::new(CmEventChannel::new()?);

                // Migrate the ID to the new channel
                tracing::debug!("Migrating ID to new channel...");
                client_id.migrate_id(new_channel.clone())?;

                // Setup resources
                let verbs = client_id.context();
                if verbs.is_null() {
                    tracing::error!("Client ID verbs context is NULL");
                }
                let device_raw = unsafe { (*verbs).device };
                let device = Arc::new(unsafe { Device::from_context(verbs, device_raw) });

                let pd = ProtectionDomain::new(device.clone())?;
                let cq = CompletionQueue::new(device.clone(), 16)?;

                let qp = QueuePair::new_cm(
                    pd.clone(),
                    client_id.id,
                    QpInitAttr {
                        send_cq: cq.clone(),
                        recv_cq: cq.clone(),
                    },
                )?;

                // Accept
                tracing::debug!("Accepting connection...");
                client_id.accept()?;

                // Wait for ESTABLISHED on the NEW channel
                tracing::debug!("Waiting for ESTABLISHED event on new channel...");
                let event = new_channel.get_event().await?;
                tracing::debug!("Received event on new channel: {}", event.event_type());
                if event.event_type() != rdma_cm_event_type::RDMA_CM_EVENT_ESTABLISHED {
                    return Err(RdmaError::Rdma(format!(
                        "Accept failed: {:?}",
                        event.event_type()
                    )));
                }
                tracing::debug!("Connection established (server side)");
                let (tx, rx) = tokio::sync::mpsc::channel(128);

                // Start the background poller for the accepted connection
                let poller_handle = RdmaStream::spawn_cq_poller(cq.clone(), rx, qp.clone());

                return Ok(RdmaStream {
                    id: client_id,
                    qp,
                    pd,
                    cq,
                    poller_handle,
                    tx,
                });
            }
        }
    }
}
