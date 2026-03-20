use crate::MemoryRegion;
use crate::cm::{CmEventChannel, CmId};
use crate::cq::CompletionQueue;
use crate::device::Device;
use crate::error::{RdmaError, Result};
use crate::pd::ProtectionDomain;
use crate::qp::{QpInitAttr, QueuePair};
use parking_lot::Mutex;
use rdma_sys::{ibv_wc, rdma_cm_event_type};
use slab::Slab;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use tokio::task::JoinHandle;

#[derive(Debug)]
pub struct WorkCompletion {
    pub wc: rdma_sys::ibv_wc,
}

pub(crate) struct WakerState {
    pub waker: Option<Waker>,
    pub result: Option<Result<ibv_wc>>,
}

pub(crate) type SharedState = Arc<Mutex<Slab<WakerState>>>;

pub struct RdmaFuture {
    shared: SharedState,
    wr_id: usize,
}

impl Future for RdmaFuture {
    type Output = Result<WorkCompletion>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.shared.lock();
        let entry = state.get_mut(self.wr_id).expect("Invalid wr_id");
        if let Some(res) = entry.result.take() {
            state.remove(self.wr_id);
            Poll::Ready(res.map(|wc| WorkCompletion { wc }))
        } else {
            entry.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

pub struct RdmaStream {
    pub id: CmId,
    pub qp: Arc<QueuePair>,
    pub pd: Arc<ProtectionDomain>,
    pub cq: Arc<CompletionQueue>,
    poller_handle: JoinHandle<()>,
    shared: SharedState,
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

        let qp = unsafe {
            QueuePair::new_cm(
                pd.clone(),
                id.id,
                QpInitAttr {
                    send_cq: cq.clone(),
                    recv_cq: cq.clone(),
                },
            )
        }?;

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
        
        let shared = Arc::new(Mutex::new(Slab::new()));
        let poller_handle = Self::spawn_cq_poller(cq.clone(), shared.clone());
        Ok(Self {
            id,
            qp,
            pd,
            cq,
            poller_handle,
            shared,
        })
    }

    pub async fn connect(addr: SocketAddr) -> Result<Self> {
        Self::connect_internal(None, addr).await
    }

    /// Helper to spawn the background polling task
    fn spawn_cq_poller(
        cq: Arc<CompletionQueue>,
        shared: SharedState,
    ) -> JoinHandle<()> {
        tokio::task::spawn(async move {
            loop {
                match cq.poll().await {
                    Ok(wc) => {
                        let wr_id = wc.wr_id as usize;
                        let mut state = shared.lock();
                        if let Some(entry) = state.get_mut(wr_id) {
                            entry.result = Some(Ok(wc));
                            if let Some(waker) = entry.waker.take() {
                                waker.wake();
                            }
                        } else {
                            tracing::warn!("no wc entry for {wc:?}");
                        }
                    }
                    Err(_e) => {
                        // Polling failed, likely CQ destroyed or device error
                        break;
                    }
                }
            }
        })
    }

    pub async fn send_multi(
        &self,
        requests: Vec<(Arc<MemoryRegion>, u64, u32)>,
    ) -> Result<WorkCompletion> {
        let wr_id = {
            let mut state = self.shared.lock();
            state.insert(WakerState { waker: None, result: None })
        };

        let reqs: Vec<_> = requests
            .iter()
            .map(|(mr, offset, len)| (mr.as_ref(), *offset, *len))
            .collect();

        if let Err(e) = unsafe { self.qp.post_send_multi(reqs, wr_id as u64, true) } {
            let mut state = self.shared.lock();
            state.remove(wr_id);
            return Err(e);
        }

        RdmaFuture {
            shared: self.shared.clone(),
            wr_id,
        }
        .await
    }

    pub async fn recv_multi(
        &self,
        requests: Vec<(Arc<MemoryRegion>, u64, u32)>,
    ) -> Result<WorkCompletion> {
        let wr_id = {
            let mut state = self.shared.lock();
            state.insert(WakerState { waker: None, result: None })
        };

        let reqs: Vec<_> = requests
            .iter()
            .map(|(mr, offset, len)| (mr.as_ref(), *offset, *len))
            .collect();

        if let Err(e) = unsafe { self.qp.post_recv_multi(reqs, wr_id as u64) } {
            let mut state = self.shared.lock();
            state.remove(wr_id);
            return Err(e);
        }

        RdmaFuture {
            shared: self.shared.clone(),
            wr_id,
        }
        .await
    }

    pub async fn send(
        &self,
        mr: Arc<MemoryRegion>,
        offset: u64,
        len: u32,
    ) -> Result<WorkCompletion> {
        self.send_multi(vec![(mr, offset, len)]).await
    }

    pub async fn recv(
        &self,
        mr: Arc<MemoryRegion>,
        offset: u64,
        len: u32,
    ) -> Result<WorkCompletion> {
        self.recv_multi(vec![(mr, offset, len)]).await
    }

    pub async fn read(
        &self,
        mr: Arc<MemoryRegion>,
        offset: u64,
        len: u32,
        remote_addr: u64,
        rkey: u32,
    ) -> Result<WorkCompletion> {
        let wr_id = {
            let mut state = self.shared.lock();
            state.insert(WakerState { waker: None, result: None })
        };

        let req = vec![(mr.as_ref(), offset, len)];

        if let Err(e) = unsafe {
            self.qp.post_rdma(
                req,
                rdma_sys::ibv_wr_opcode::IBV_WR_RDMA_READ,
                wr_id as u64,
                true,
                remote_addr,
                rkey,
            )
        } {
            let mut state = self.shared.lock();
            state.remove(wr_id);
            return Err(e);
        }

        RdmaFuture {
            shared: self.shared.clone(),
            wr_id,
        }
        .await
    }

    pub async fn write(
        &self,
        mr: Arc<MemoryRegion>,
        offset: u64,
        len: u32,
        remote_addr: u64,
        rkey: u32,
    ) -> Result<WorkCompletion> {
        let wr_id = {
            let mut state = self.shared.lock();
            state.insert(WakerState { waker: None, result: None })
        };

        let req = vec![(mr.as_ref(), offset, len)];

        if let Err(e) = unsafe {
            self.qp.post_rdma(
                req,
                rdma_sys::ibv_wr_opcode::IBV_WR_RDMA_WRITE,
                wr_id as u64,
                true,
                remote_addr,
                rkey,
            )
        } {
            let mut state = self.shared.lock();
            state.remove(wr_id);
            return Err(e);
        }

        RdmaFuture {
            shared: self.shared.clone(),
            wr_id,
        }
        .await
    }

    pub fn register_mr(&self, data: Vec<u8>, access: i32) -> Result<Arc<MemoryRegion>> {
        MemoryRegion::register(self.pd.clone(), data, access)
    }

    pub fn register_dmabuf_mr(
        &self,
        dmabuf: crate::DmaBuf,
        offset: u64,
        access: i32,
    ) -> Result<Arc<MemoryRegion>> {
        MemoryRegion::register_dmabuf(self.pd.clone(), dmabuf, offset, access)
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

                let qp = unsafe {
                    QueuePair::new_cm(
                        pd.clone(),
                        client_id.id,
                        QpInitAttr {
                            send_cq: cq.clone(),
                            recv_cq: cq.clone(),
                        },
                    )
                }?;

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
                
                let shared = Arc::new(Mutex::new(Slab::new()));

                // Start the background poller for the accepted connection
                let poller_handle = RdmaStream::spawn_cq_poller(cq.clone(), shared.clone());

                return Ok(RdmaStream {
                    id: client_id,
                    qp,
                    pd,
                    cq,
                    poller_handle,
                    shared,
                });
            }
        }
    }
}