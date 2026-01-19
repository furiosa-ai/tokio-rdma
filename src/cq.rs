use crate::device::Device;
use crate::error::{RdmaError, Result};
use rdma_sys::*;
use std::os::unix::io::RawFd;
use std::ptr;
use std::sync::Arc;
use tokio::io::unix::AsyncFd;

pub struct CompletionQueue {
    pub(crate) cq: *mut ibv_cq,
    channel: *mut ibv_comp_channel,
    poll_fd: AsyncFd<RawFd>,
    _device: Arc<Device>,
}

unsafe impl Send for CompletionQueue {}
unsafe impl Sync for CompletionQueue {}

impl CompletionQueue {
    pub fn new(device: Arc<Device>, cqe: i32) -> Result<Arc<Self>> {
        let channel = unsafe { ibv_create_comp_channel(device.context) };
        if channel.is_null() {
            return Err(RdmaError::Rdma(
                "Failed to create completion channel".into(),
            ));
        }

        let cq = unsafe { ibv_create_cq(device.context, cqe, ptr::null_mut(), channel, 0) };

        if cq.is_null() {
            unsafe { ibv_destroy_comp_channel(channel) };
            return Err(RdmaError::Rdma("Failed to create CQ".into()));
        }

        // Make the channel non-blocking?
        // ibv_get_cq_event blocks. But if we use AsyncFd, we only call it when readable.
        // Actually, it's safer to set the fd to non-blocking mode.
        let fd = unsafe { (*channel).fd };

        let mut flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags != -1 {
            flags |= libc::O_NONBLOCK;
            unsafe { libc::fcntl(fd, libc::F_SETFL, flags) };
        }

        let poll_fd = AsyncFd::new(fd)?;

        // Request notifications initially? Usually we do it when we want to sleep.

        Ok(Arc::new(Self {
            cq,
            channel,
            poll_fd,
            _device: device,
        }))
    }

    pub async fn poll(&self) -> Result<ibv_wc> {
        loop {
            // 1. Try to poll
            let mut wc: ibv_wc = unsafe { std::mem::zeroed() };
            let ret = unsafe { ibv_poll_cq(self.cq, 1, &mut wc) };

            if ret > 0 {
                return Ok(wc);
            } else if ret < 0 {
                return Err(RdmaError::Rdma("Poll CQ failed".into()));
            }

            // 2. Request notification
            let ret = unsafe { ibv_req_notify_cq(self.cq, 0) };
            if ret != 0 {
                return Err(RdmaError::Rdma("Req notify failed".into()));
            }

            // 3. Poll again to avoid race
            let ret = unsafe { ibv_poll_cq(self.cq, 1, &mut wc) };
            if ret > 0 {
                return Ok(wc);
            }

            // 4. Wait for event
            let mut guard = self.poll_fd.readable().await?;

            let mut event_cq: *mut ibv_cq = ptr::null_mut();
            let mut event_context: *mut std::ffi::c_void = ptr::null_mut();

            let ret = unsafe { ibv_get_cq_event(self.channel, &mut event_cq, &mut event_context) };
            if ret == 0 {
                unsafe { ibv_ack_cq_events(event_cq, 1) };
                guard.clear_ready();
            } else {
                // If errno is EAGAIN, it's fine, but with AsyncFd logic, we shouldn't be here unless ready.
                // However, if we fail to read, we should check errno.
                // Since we set O_NONBLOCK, it returns -1 and sets errno to EAGAIN if empty.
                let errno = unsafe { *libc::__errno_location() };
                if errno != libc::EAGAIN {
                    return Err(RdmaError::Rdma("Get CQ event failed".into()));
                }
                guard.clear_ready();
            }
        }
    }
}

impl Drop for CompletionQueue {
    fn drop(&mut self) {
        unsafe {
            ibv_destroy_cq(self.cq);
            ibv_destroy_comp_channel(self.channel);
        }
    }
}
