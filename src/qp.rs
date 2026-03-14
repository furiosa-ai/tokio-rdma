use crate::cq::CompletionQueue;
use crate::error::{RdmaError, Result};
use crate::mr::MemoryRegion;
use crate::pd::ProtectionDomain;
use rdma_sys::*;
use std::ptr;
use std::sync::Arc;

pub struct QueuePair {
    qp: *mut ibv_qp,
    _pd: Arc<ProtectionDomain>,
    _scq: Arc<CompletionQueue>,
    _rcq: Arc<CompletionQueue>,
}

unsafe impl Send for QueuePair {}
unsafe impl Sync for QueuePair {}

pub struct QpInitAttr {
    pub send_cq: Arc<CompletionQueue>,
    pub recv_cq: Arc<CompletionQueue>,
    // Simplification: use reliable connected (RC)
}

impl QueuePair {
    /// Creates a new Queue Pair associated with a CM ID.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `cm_id` is a valid pointer to a `rdma_cm_id` and that it stays
    /// valid for the duration of this call.
    pub unsafe fn new_cm(
        pd: Arc<ProtectionDomain>,
        cm_id: *mut rdma_cm_id,
        init_attr: QpInitAttr,
    ) -> Result<Arc<Self>> {
        let mut attr: ibv_qp_init_attr = unsafe { std::mem::zeroed() };
        attr.qp_type = ibv_qp_type::IBV_QPT_RC;
        attr.sq_sig_all = 0;
        attr.send_cq = init_attr.send_cq.cq;
        attr.recv_cq = init_attr.recv_cq.cq;

        attr.cap.max_send_wr = 128;
        attr.cap.max_recv_wr = 128;
        attr.cap.max_send_sge = 1;
        attr.cap.max_recv_sge = 1;

        let ret = unsafe { rdma_create_qp(cm_id, pd.pd, &mut attr) };
        if ret != 0 {
            return Err(RdmaError::Rdma(format!("Failed to create CM QP: {}", ret)));
        }

        let qp = unsafe { (*cm_id).qp };

        Ok(Arc::new(Self {
            qp,
            _pd: pd,
            _scq: init_attr.send_cq,
            _rcq: init_attr.recv_cq,
        }))
    }

    pub fn qp_num(&self) -> u32 {
        unsafe { (*self.qp).qp_num }
    }

    // Helper to transition to INIT
    pub fn to_init(&self, port_num: u8) -> Result<()> {
        let mut attr: ibv_qp_attr = unsafe { std::mem::zeroed() };
        attr.qp_state = ibv_qp_state::IBV_QPS_INIT;
        attr.pkey_index = 0;
        attr.port_num = port_num;
        attr.qp_access_flags = ibv_access_flags::IBV_ACCESS_LOCAL_WRITE.0
            | ibv_access_flags::IBV_ACCESS_REMOTE_READ.0
            | ibv_access_flags::IBV_ACCESS_REMOTE_WRITE.0;

        let mask = ibv_qp_attr_mask::IBV_QP_STATE.0
            | ibv_qp_attr_mask::IBV_QP_PKEY_INDEX.0
            | ibv_qp_attr_mask::IBV_QP_PORT.0
            | ibv_qp_attr_mask::IBV_QP_ACCESS_FLAGS.0;

        let ret = unsafe { ibv_modify_qp(self.qp, &mut attr, mask as i32) };
        if ret != 0 {
            return Err(RdmaError::Rdma(format!(
                "Failed to modify QP to INIT: {}",
                ret
            )));
        }
        Ok(())
    }

    // Helper to transition to RTR
    pub fn to_rtr(
        &self,
        dest_qpn: u32,
        dest_lid: u16,
        port_num: u8,
        dest_psn: u32,
        dest_gid: Option<[u8; 16]>,
    ) -> Result<()> {
        let mut attr: ibv_qp_attr = unsafe { std::mem::zeroed() };
        attr.qp_state = ibv_qp_state::IBV_QPS_RTR;
        attr.path_mtu = ibv_mtu::IBV_MTU_1024;
        attr.dest_qp_num = dest_qpn;
        attr.rq_psn = dest_psn;
        attr.max_dest_rd_atomic = 1;
        attr.min_rnr_timer = 12;

        attr.ah_attr.dlid = dest_lid;
        attr.ah_attr.sl = 0;
        attr.ah_attr.src_path_bits = 0;
        attr.ah_attr.port_num = port_num;

        if let Some(gid) = dest_gid {
            attr.ah_attr.is_global = 1;
            attr.ah_attr.grh.dgid.raw = gid;
            attr.ah_attr.grh.sgid_index = 0;
            attr.ah_attr.grh.hop_limit = 1;
        } else {
            attr.ah_attr.is_global = 0;
        }

        let mask = ibv_qp_attr_mask::IBV_QP_STATE.0
            | ibv_qp_attr_mask::IBV_QP_AV.0
            | ibv_qp_attr_mask::IBV_QP_PATH_MTU.0
            | ibv_qp_attr_mask::IBV_QP_DEST_QPN.0
            | ibv_qp_attr_mask::IBV_QP_RQ_PSN.0
            | ibv_qp_attr_mask::IBV_QP_MAX_DEST_RD_ATOMIC.0
            | ibv_qp_attr_mask::IBV_QP_MIN_RNR_TIMER.0;

        let ret = unsafe { ibv_modify_qp(self.qp, &mut attr, mask as i32) };
        if ret != 0 {
            return Err(RdmaError::Rdma(format!(
                "Failed to modify QP to RTR: {}",
                ret
            )));
        }
        Ok(())
    }

    // Helper to transition to RTS
    pub fn to_rts(&self, my_psn: u32) -> Result<()> {
        let mut attr: ibv_qp_attr = unsafe { std::mem::zeroed() };
        attr.qp_state = ibv_qp_state::IBV_QPS_RTS;
        attr.timeout = 14;
        attr.retry_cnt = 7;
        attr.rnr_retry = 7;
        attr.sq_psn = my_psn;
        attr.max_rd_atomic = 1;

        let mask = ibv_qp_attr_mask::IBV_QP_STATE.0
            | ibv_qp_attr_mask::IBV_QP_TIMEOUT.0
            | ibv_qp_attr_mask::IBV_QP_RETRY_CNT.0
            | ibv_qp_attr_mask::IBV_QP_RNR_RETRY.0
            | ibv_qp_attr_mask::IBV_QP_SQ_PSN.0
            | ibv_qp_attr_mask::IBV_QP_MAX_QP_RD_ATOMIC.0;

        let ret = unsafe { ibv_modify_qp(self.qp, &mut attr, mask as i32) };
        if ret != 0 {
            return Err(RdmaError::Rdma(format!(
                "Failed to modify QP to RTS: {}",
                ret
            )));
        }
        Ok(())
    }

    /// Posts a receive request.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the memory regions stay valid until the completion event is
    /// received.
    pub unsafe fn post_recv_multi(
        &self,
        req: Vec<(&MemoryRegion, u64, u32)>,
        wr_id: u64,
    ) -> Result<()> {
        let mut sges: Vec<_> = req
            .iter()
            .map(|(mr, offset, len)| {
                let mut sge: ibv_sge = unsafe { std::mem::zeroed() };
                sge.addr = mr.addr() + offset;
                sge.length = *len;
                sge.lkey = mr.lkey();
                sge
            })
            .collect();

        let mut wr: ibv_recv_wr = unsafe { std::mem::zeroed() };
        wr.wr_id = wr_id;
        wr.sg_list = sges.as_mut_ptr();
        wr.num_sge = sges.len().try_into().unwrap();
        wr.next = ptr::null_mut();

        let mut bad_wr: *mut ibv_recv_wr = ptr::null_mut();

        let ret = unsafe { ibv_post_recv(self.qp, &mut wr, &mut bad_wr) };
        if ret != 0 {
            return Err(RdmaError::Rdma(format!("Failed to post recv : {}", ret)));
        }
        Ok(())
    }

    /// Posts a send request.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the memory regions stay valid until the completion event is
    /// received.
    pub unsafe fn post_send_multi(
        &self,
        req: Vec<(&MemoryRegion, u64, u32)>,
        wr_id: u64,
        signaled: bool,
    ) -> Result<()> {
        let mut sges: Vec<_> = req
            .iter()
            .map(|(mr, offset, len)| {
                let mut sge: ibv_sge = unsafe { std::mem::zeroed() };
                sge.addr = mr.addr() + offset;
                sge.length = *len;
                sge.lkey = mr.lkey();
                sge
            })
            .collect();

        let mut wr: ibv_send_wr = unsafe { std::mem::zeroed() };
        wr.wr_id = wr_id;
        wr.sg_list = sges.as_mut_ptr();
        wr.num_sge = sges.len().try_into().unwrap();
        wr.opcode = ibv_wr_opcode::IBV_WR_SEND;
        wr.send_flags = if signaled {
            ibv_send_flags::IBV_SEND_SIGNALED.0
        } else {
            0
        };
        wr.next = ptr::null_mut();

        let mut bad_wr: *mut ibv_send_wr = ptr::null_mut();

        let ret = unsafe { ibv_post_send(self.qp, &mut wr, &mut bad_wr) };
        if ret != 0 {
            return Err(RdmaError::Rdma(format!("Failed to post send: {}", ret)));
        }
        Ok(())
    }

    /// Posts an RDMA request.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the memory regions stay valid until the completion event is
    /// received.
    pub unsafe fn post_rdma(
        &self,
        req: Vec<(&MemoryRegion, u64, u32)>,
        op: ibv_wr_opcode::Type,
        wr_id: u64,
        signaled: bool,
        remote_addr: u64,
        rkey: u32,
    ) -> Result<()> {
        let mut sges: Vec<_> = req
            .iter()
            .map(|(mr, offset, len)| {
                let mut sge: ibv_sge = unsafe { std::mem::zeroed() };
                sge.addr = mr.addr() + offset;
                sge.length = *len;
                sge.lkey = mr.lkey();
                sge
            })
            .collect();

        let mut wr: ibv_send_wr = unsafe { std::mem::zeroed() };
        wr.wr_id = wr_id;
        wr.sg_list = sges.as_mut_ptr();
        wr.num_sge = sges.len().try_into().unwrap();
        wr.opcode = op;
        wr.send_flags = if signaled {
            ibv_send_flags::IBV_SEND_SIGNALED.0
        } else {
            0
        };
        wr.wr.rdma.remote_addr = remote_addr;
        wr.wr.rdma.rkey = rkey;
        wr.next = ptr::null_mut();

        let mut bad_wr: *mut ibv_send_wr = ptr::null_mut();

        let ret = unsafe { ibv_post_send(self.qp, &mut wr, &mut bad_wr) };
        if ret != 0 {
            return Err(RdmaError::Rdma(format!("Failed to post rdma: {}", ret)));
        }
        Ok(())
    }
}

impl Drop for QueuePair {
    fn drop(&mut self) {
        unsafe { ibv_destroy_qp(self.qp) };
    }
}
