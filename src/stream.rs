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
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;

pub struct RdmaStream {
    pub id: CmId,
    pub qp: Arc<QueuePair>,
    pub pd: Arc<ProtectionDomain>,
    pub cq: Arc<CompletionQueue>,
    wr_id: AtomicU64,
    // Map from wr_id to a oneshot sender that will notify the waiting task
    works: Arc<Mutex<HashMap<u64, oneshot::Sender<ibv_wc>>>>,
    poller_handle: JoinHandle<()>,
}

impl Drop for RdmaStream {
    fn drop(&mut self) {
        self.poller_handle.abort();
    }
}

impl RdmaStream {
    pub async fn connect(addr: SocketAddr) -> Result<Self> {
        // 1. Setup Channel & ID
        let channel = Arc::new(CmEventChannel::new()?);
        let id = CmId::new(channel.clone())?;

        // 2. Resolve Address
        id.resolve_addr(addr)?;
        let event = channel.get_event().await?;
        if event.event_type() != rdma_cm_event_type::RDMA_CM_EVENT_ADDR_RESOLVED {
            return Err(RdmaError::Rdma(format!(
                "Addr resolution failed: {:?}",
                event.event_type()
            )));
        }

        // 3. Resolve Route
        id.resolve_route()?;
        let event = channel.get_event().await?;
        if event.event_type() != rdma_cm_event_type::RDMA_CM_EVENT_ROUTE_RESOLVED {
            return Err(RdmaError::Rdma(format!(
                "Route resolution failed: {:?}",
                event.event_type()
            )));
        }

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
        id.connect()?;
        let event = channel.get_event().await?;
        if event.event_type() != rdma_cm_event_type::RDMA_CM_EVENT_ESTABLISHED {
            return Err(RdmaError::Rdma(format!(
                "Connection failed: {:?}",
                event.event_type()
            )));
        }

        let wr_id = AtomicU64::new(0);
        let works = Arc::new(Mutex::new(HashMap::new()));

        let poller_handle = Self::spawn_cq_poller(cq.clone(), works.clone());

        Ok(Self {
            id,
            qp,
            pd,
            cq,
            wr_id,
            works,
            poller_handle,
        })
    }

    /// Helper to spawn the background polling task
    fn spawn_cq_poller(
        cq: Arc<CompletionQueue>,
        works: Arc<Mutex<HashMap<u64, oneshot::Sender<ibv_wc>>>>,
    ) -> JoinHandle<()> {
        tokio::task::spawn(async move {
            loop {
                match cq.poll().await {
                    Ok(wc) => {
                        let mut locked = works.lock().await;
                        if let Some(tx) = locked.remove(&wc.wr_id) {
                            // Send the completion to the waiting task
                            let _ = tx.send(wc);
                        } else {
                            // This might happen if the sender was dropped or cancelled
                            // or if we have a spurious completion.
                            // For now, we just ignore it.
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

    pub async fn send(&mut self, mr: &MemoryRegion, offset: u64, len: u32) -> Result<ibv_wc> {
        let wr_id = self.wr_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();

        // Insert into the map BEFORE posting send to avoid race conditions
        {
            let mut locked = self.works.lock().await;
            locked.insert(wr_id, tx);
        }

        unsafe {
            if let Err(e) = self.qp.post_send(mr, offset, len, wr_id, true) {
                // If posting fails, remove the entry so we don't leak memory in the map
                let mut locked = self.works.lock().await;
                locked.remove(&wr_id);
                return Err(e);
            }
        }

        // Wait for the completion
        match rx.await {
            Ok(wc) => Ok(wc),
            Err(_) => Err(RdmaError::Rdma(
                "CQ Poller task failed or dropped channel".into(),
            )),
        }
    }

    pub async fn recv(&mut self, mr: &MemoryRegion, offset: u64, len: u32) -> Result<ibv_wc> {
        let wr_id = self.wr_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();

        // Insert into the map BEFORE posting recv to avoid race conditions
        {
            let mut locked = self.works.lock().await;
            locked.insert(wr_id, tx);
        }

        unsafe {
            if let Err(e) = self.qp.post_recv(mr, offset, len, wr_id) {
                // If posting fails, remove the entry so we don't leak memory in the map
                let mut locked = self.works.lock().await;
                locked.remove(&wr_id);
                return Err(e);
            }
        }

        // Wait for the completion
        match rx.await {
            Ok(wc) => Ok(wc),
            Err(_) => Err(RdmaError::Rdma(
                "CQ Poller task failed or dropped channel".into(),
            )),
        }
    }
}

pub struct RdmaListener {
    pub listener_id: CmId,
    pub channel: Arc<CmEventChannel>,
}

impl RdmaListener {
    pub async fn bind(addr: SocketAddr) -> Result<Self> {
        let channel = Arc::new(CmEventChannel::new()?);
        let listener_id = CmId::new(channel.clone())?;
        listener_id.bind(addr)?;
        listener_id.listen(1)?;
        Ok(Self {
            listener_id,
            channel,
        })
    }

    pub async fn accept(&self) -> Result<RdmaStream> {
        loop {
            let event = self.channel.get_event().await?;
            if event.event_type() == rdma_cm_event_type::RDMA_CM_EVENT_CONNECT_REQUEST {
                let client_id_raw = event.id();
                // Wrap the ID initially with the listener's channel
                let mut client_id =
                    unsafe { CmId::from_raw(client_id_raw, Some(self.channel.clone())) };

                // Create a NEW channel for this connection to isolate it
                let new_channel = Arc::new(CmEventChannel::new()?);

                // Migrate the ID to the new channel
                client_id.migrate_id(new_channel.clone())?;

                // Setup resources
                let verbs = client_id.context();
                let device_raw = unsafe { (*verbs).device };
                let device = Arc::new(unsafe { Device::from_context(verbs, device_raw) });

                let pd = ProtectionDomain::new(device.clone())?;
                // Default CQ size 16 for now
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
                client_id.accept()?;

                // Wait for ESTABLISHED on the NEW channel
                let event = new_channel.get_event().await?;
                if event.event_type() != rdma_cm_event_type::RDMA_CM_EVENT_ESTABLISHED {
                    return Err(RdmaError::Rdma(format!(
                        "Accept failed: {:?}",
                        event.event_type()
                    )));
                }

                let wr_id = AtomicU64::new(0);
                let works = Arc::new(Mutex::new(HashMap::new()));

                // Start the background poller for the accepted connection
                let poller_handle = RdmaStream::spawn_cq_poller(cq.clone(), works.clone());

                return Ok(RdmaStream {
                    id: client_id,
                    qp,
                    pd,
                    cq,
                    wr_id,
                    works,
                    poller_handle,
                });
            }
        }
    }
}
