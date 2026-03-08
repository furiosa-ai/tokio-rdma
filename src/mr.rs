use crate::error::{RdmaError, Result};
use crate::pd::ProtectionDomain;
use rdma_sys::*;
use std::ffi::c_void;
use std::sync::Arc;

use std::os::fd::AsRawFd;

pub trait DmaBuf: AsRawFd {
    fn dmabuf_offset(&self) -> u64;
    fn dmabuf_length(&self) -> usize;
}

pub struct MemoryRegion {
    pub(crate) mr: *mut ibv_mr,
    _pd: Arc<ProtectionDomain>,
    _data: Option<Vec<u8>>, // Only present if we own the allocation
}

impl std::fmt::Debug for MemoryRegion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        unsafe { *self.mr }.fmt(f)
    }
}

impl MemoryRegion {
    /// Registers an existing memory block (e.g. mmap'd P2P memory)
    pub unsafe fn register_user(
        pd: Arc<ProtectionDomain>,
        addr: *mut c_void,
        len: usize,
        access: i32,
    ) -> Result<Self> {
        let mr = unsafe { ibv_reg_mr(pd.pd, addr, len, access) };
        if mr.is_null() {
            let errno = unsafe { *libc::__errno_location() };
            return Err(RdmaError::Rdma(format!(
                "Failed to register external MR: errno {}",
                errno
            )));
        }
        Ok(Self {
            mr,
            _pd: pd,
            _data: None,
        })
    }

    pub fn register_dmabuf(
        pd: Arc<ProtectionDomain>,
        dmabuf: &impl DmaBuf,
        access: i32,
    ) -> Result<Arc<Self>> {
        let mr = unsafe {
            ibv_reg_dmabuf_mr(
                pd.pd,
                dmabuf.dmabuf_offset(),
                dmabuf.dmabuf_length(),
                0, // iova
                dmabuf.as_raw_fd(),
                access,
            )
        };

        if mr.is_null() {
            let errno = unsafe { *libc::__errno_location() };
            return Err(RdmaError::Rdma(format!(
                "Failed to register dmabuf MR: errno {}",
                errno
            )));
        }

        Ok(Arc::new(Self {
            mr,
            _pd: pd,
            _data: None,
        }))
    }

    pub fn register(pd: Arc<ProtectionDomain>, len: usize) -> Result<Arc<Self>> {
        let mut data = vec![0u8; len];
        let access = ibv_access_flags::IBV_ACCESS_LOCAL_WRITE.0
            | ibv_access_flags::IBV_ACCESS_REMOTE_WRITE.0
            | ibv_access_flags::IBV_ACCESS_REMOTE_READ.0;

        let mr = unsafe { ibv_reg_mr(pd.pd, data.as_mut_ptr() as *mut c_void, len, access as i32) };

        if mr.is_null() {
            let errno = unsafe { *libc::__errno_location() };
            return Err(RdmaError::Rdma(format!(
                "Failed to register MR: errno {}",
                errno
            )));
        }

        Ok(Arc::new(Self {
            mr,
            _pd: pd,
            _data: Some(data),
        }))
    }

    pub fn rkey(&self) -> u32 {
        unsafe { (*self.mr).rkey }
    }

    pub fn lkey(&self) -> u32 {
        unsafe { (*self.mr).lkey }
    }

    pub fn addr(&self) -> u64 {
        unsafe { (*self.mr).addr as u64 }
    }

    // Unsafe access to underlying buffer
    pub unsafe fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts((*self.mr).addr as *const u8, (*self.mr).length) }
    }

    pub unsafe fn as_mut_slice(&self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut((*self.mr).addr as *mut u8, (*self.mr).length) }
    }

    pub fn len(&self) -> usize {
        unsafe { *self.mr }.length
    }
}

impl Drop for MemoryRegion {
    fn drop(&mut self) {
        unsafe { ibv_dereg_mr(self.mr) };
    }
}

unsafe impl Send for MemoryRegion {}
unsafe impl Sync for MemoryRegion {}
