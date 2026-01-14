use crate::device::Device;
use crate::error::{RdmaError, Result};
use rdma_sys::*;
use std::sync::Arc;

pub struct ProtectionDomain {
    pub(crate) pd: *mut ibv_pd,
    _device: Arc<Device>, // Keep device alive
}

impl ProtectionDomain {
    pub fn new(device: Arc<Device>) -> Result<Arc<Self>> {
        let pd = unsafe { ibv_alloc_pd(device.context) };
        if pd.is_null() {
            return Err(RdmaError::Rdma("Failed to allocate PD".into()));
        }
        Ok(Arc::new(Self {
            pd,
            _device: device,
        }))
    }
}

impl Drop for ProtectionDomain {
    fn drop(&mut self) {
        unsafe { ibv_dealloc_pd(self.pd) };
    }
}

unsafe impl Send for ProtectionDomain {}
unsafe impl Sync for ProtectionDomain {}
