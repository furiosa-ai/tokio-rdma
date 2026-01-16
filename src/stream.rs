use crate::cm::{CmEventChannel, CmId};
use crate::cq::CompletionQueue;
use crate::device::Device;
use crate::error::{RdmaError, Result};
use crate::pd::ProtectionDomain;
use crate::qp::{QpInitAttr, QueuePair};
use rdma_sys::rdma_cm_event_type;
use std::net::SocketAddr;
use std::sync::Arc;

pub struct RdmaStream {
    pub id: CmId,
    pub qp: Arc<QueuePair>,
    pub pd: Arc<ProtectionDomain>,
    pub cq: Arc<CompletionQueue>,
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

        Ok(Self { id, qp, pd, cq })
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

                return Ok(RdmaStream {
                    id: client_id,
                    qp,
                    pd,
                    cq,
                });
            }
        }
    }
}
