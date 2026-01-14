use crate::error::{RdmaError, Result};
use rdma_sys::*;
use std::net::SocketAddr;
use std::os::unix::io::RawFd;
use std::ptr;
use std::sync::Arc;
use tokio::io::unix::AsyncFd;

pub struct CmEventChannel {
    pub(crate) channel: *mut rdma_event_channel,
    async_fd: AsyncFd<RawFd>,
}

impl CmEventChannel {
    pub fn new() -> Result<Self> {
        let channel = unsafe { rdma_create_event_channel() };
        if channel.is_null() {
            return Err(RdmaError::Rdma("Failed to create CM event channel".into()));
        }

        let fd = unsafe { (*channel).fd };
        // Set non-blocking
        let mut flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags != -1 {
            flags |= libc::O_NONBLOCK;
            unsafe { libc::fcntl(fd, libc::F_SETFL, flags) };
        }

        let async_fd = AsyncFd::new(fd)?;
        Ok(Self { channel, async_fd })
    }

    pub async fn get_event(&self) -> Result<CmEvent> {
        loop {
            let mut event: *mut rdma_cm_event = ptr::null_mut();
            let ret = unsafe { rdma_get_cm_event(self.channel, &mut event) };

            if ret == 0 {
                return Ok(CmEvent { event });
            } else {
                let errno = unsafe { *libc::__errno_location() };
                if errno == libc::EAGAIN || errno == libc::EWOULDBLOCK {
                    let mut guard = self.async_fd.readable().await?;
                    guard.clear_ready();
                } else {
                    return Err(RdmaError::Rdma(format!(
                        "Failed to get CM event: {}",
                        errno
                    )));
                }
            }
        }
    }
}

impl Drop for CmEventChannel {
    fn drop(&mut self) {
        unsafe { rdma_destroy_event_channel(self.channel) };
    }
}

pub struct CmEvent {
    pub(crate) event: *mut rdma_cm_event,
}

impl CmEvent {
    pub fn event_type(&self) -> rdma_cm_event_type::Type {
        unsafe { (*self.event).event }
    }

    pub fn id(&self) -> *mut rdma_cm_id {
        unsafe { (*self.event).id }
    }

    pub fn listen_id(&self) -> *mut rdma_cm_id {
        unsafe { (*self.event).listen_id }
    }
}

impl Drop for CmEvent {
    fn drop(&mut self) {
        unsafe { rdma_ack_cm_event(self.event) };
    }
}

pub struct CmId {
    pub id: *mut rdma_cm_id,
    _channel: Option<Arc<CmEventChannel>>,
}

impl CmId {
    pub fn new(channel: Arc<CmEventChannel>) -> Result<Self> {
        let mut id: *mut rdma_cm_id = ptr::null_mut();
        let ret = unsafe {
            rdma_create_id(
                channel.channel,
                &mut id,
                ptr::null_mut(),
                rdma_port_space::RDMA_PS_TCP,
            )
        };
        if ret != 0 {
            return Err(RdmaError::Rdma("Failed to create CM ID".into()));
        }
        Ok(Self {
            id,
            _channel: Some(channel),
        })
    }

    pub fn bind(&self, addr: SocketAddr) -> Result<()> {
        let sockaddr = socket2::SockAddr::from(addr);
        let ret = unsafe { rdma_bind_addr(self.id, sockaddr.as_ptr() as *mut _) };
        if ret != 0 {
            let errno = unsafe { *libc::__errno_location() };
            return Err(RdmaError::Rdma(format!(
                "Failed to bind CM ID: {} (errno {})",
                ret, errno
            )));
        }
        Ok(())
    }

    pub fn listen(&self, backlog: i32) -> Result<()> {
        let ret = unsafe { rdma_listen(self.id, backlog) };
        if ret != 0 {
            let errno = unsafe { *libc::__errno_location() };
            return Err(RdmaError::Rdma(format!(
                "Failed to listen on CM ID: {} (errno {})",
                ret, errno
            )));
        }
        Ok(())
    }

    pub fn resolve_addr(&self, addr: SocketAddr) -> Result<()> {
        let sockaddr = socket2::SockAddr::from(addr);
        let ret = unsafe {
            rdma_resolve_addr(self.id, ptr::null_mut(), sockaddr.as_ptr() as *mut _, 2000)
        };
        if ret != 0 {
            let errno = unsafe { *libc::__errno_location() };
            return Err(RdmaError::Rdma(format!(
                "Failed to resolve CM addr: {} (errno {})",
                ret, errno
            )));
        }
        Ok(())
    }

    pub fn resolve_route(&self) -> Result<()> {
        let ret = unsafe { rdma_resolve_route(self.id, 2000) };
        if ret != 0 {
            let errno = unsafe { *libc::__errno_location() };
            return Err(RdmaError::Rdma(format!(
                "Failed to resolve CM route: {} (errno {})",
                ret, errno
            )));
        }
        Ok(())
    }

    pub fn connect(&self) -> Result<()> {
        let mut conn_param: rdma_conn_param = unsafe { std::mem::zeroed() };
        conn_param.responder_resources = 1;
        conn_param.initiator_depth = 1;
        conn_param.retry_count = 7;

        let ret = unsafe { rdma_connect(self.id, &mut conn_param) };
        if ret != 0 {
            let errno = unsafe { *libc::__errno_location() };
            return Err(RdmaError::Rdma(format!(
                "Failed to connect CM ID: {} (errno {})",
                ret, errno
            )));
        }
        Ok(())
    }

    pub fn accept(&self) -> Result<()> {
        let mut conn_param: rdma_conn_param = unsafe { std::mem::zeroed() };
        conn_param.responder_resources = 1;
        conn_param.initiator_depth = 1;

        let ret = unsafe { rdma_accept(self.id, &mut conn_param) };
        if ret != 0 {
            let errno = unsafe { *libc::__errno_location() };
            return Err(RdmaError::Rdma(format!(
                "Failed to accept CM ID: {} (errno {})",
                ret, errno
            )));
        }
        Ok(())
    }

    pub fn context(&self) -> *mut ibv_context {
        unsafe { (*self.id).verbs }
    }

    pub fn pd(&self) -> *mut ibv_pd {
        unsafe { (*self.id).pd }
    }

    pub fn qp(&self) -> *mut ibv_qp {
        unsafe { (*self.id).qp }
    }
}

impl Drop for CmId {
    fn drop(&mut self) {
        unsafe { rdma_destroy_id(self.id) };
    }
}

unsafe impl Send for CmId {}
unsafe impl Sync for CmId {}
unsafe impl Send for CmEventChannel {}
unsafe impl Sync for CmEventChannel {}
