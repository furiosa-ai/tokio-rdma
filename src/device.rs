use crate::error::{RdmaError, Result};
use crate::utils;
use rdma_sys::*;
use std::sync::Arc;

pub struct DeviceList {
    list: *mut *mut ibv_device,
    count: i32,
}

impl DeviceList {
    pub fn available() -> Result<Self> {
        let mut count = 0;
        let list = unsafe { ibv_get_device_list(&mut count) };
        if list.is_null() {
            return Err(RdmaError::Rdma("Failed to get device list".into()));
        }
        Ok(Self { list, count })
    }

    pub fn get(&self, index: usize) -> Option<Device> {
        if index >= self.count as usize {
            return None;
        }
        let dev_ptr = unsafe { *self.list.add(index) };
        if dev_ptr.is_null() {
            return None;
        }
        Some(Device::new(dev_ptr))
    }

    pub fn find_by_name(&self, name: &str) -> Option<Device> {
        for i in 0..self.count as usize {
            if let Some(dev) = self.get(i) {
                if dev.name() == name {
                    return Some(dev);
                }
            }
        }
        None
    }

    pub fn print(&self) {
        for i in 0..self.count as usize {
            if let Some(dev) = self.get(i) {
                println!("{}", dev.name());
            }
        }
    }
}

impl Drop for DeviceList {
    fn drop(&mut self) {
        unsafe { ibv_free_device_list(self.list) };
    }
}

pub struct Device {
    pub raw: *mut ibv_device,
    pub context: *mut ibv_context,
    owned: bool,
}

impl Device {
    fn new(raw: *mut ibv_device) -> Self {
        // We don't open context here immediately to keep Device lightweight,
        // but for simplicity in this example, we open it on first use or require it.
        // Actually, let's open it on creation to ensure it works.
        let context = unsafe { ibv_open_device(raw) };
        // Note: Realistically handle error
        Self {
            raw,
            context,
            owned: true,
        }
    }

    /// Creates a Device wrapper around an existing context.
    /// The context will NOT be closed when this Device is dropped.
    pub unsafe fn from_context(context: *mut ibv_context, raw: *mut ibv_device) -> Self {
        Self {
            raw,
            context,
            owned: false,
        }
    }

    pub fn open(name: Option<&str>) -> Result<Arc<Self>> {
        let list = DeviceList::available()?;

        let dev = if let Some(n) = name {
            list.find_by_name(n).ok_or(RdmaError::DeviceNotFound)?
        } else {
            list.get(0).ok_or(RdmaError::DeviceNotFound)?
        };

        if dev.context.is_null() {
            return Err(RdmaError::Rdma("Failed to open device context".into()));
        }

        Ok(Arc::new(dev))
    }

    pub fn name(&self) -> String {
        unsafe { utils::ptr_to_string(ibv_get_device_name(self.raw)) }
    }

    pub fn get_port_attr(&self, port_num: u8) -> Result<ibv_port_attr> {
        let mut attr: ibv_port_attr = unsafe { std::mem::zeroed() };
        let ret = unsafe {
            ibv_query_port(
                self.context,
                port_num,
                &mut attr as *mut ibv_port_attr as *mut _compat_ibv_port_attr,
            )
        };
        if ret != 0 {
            return Err(RdmaError::Rdma("Failed to query port".into()));
        }
        Ok(attr)
    }

    pub fn get_gid(&self, port_num: u8, gid_index: i32) -> Result<ibv_gid> {
        let mut gid: ibv_gid = unsafe { std::mem::zeroed() };
        let ret = unsafe { ibv_query_gid(self.context, port_num, gid_index, &mut gid) };
        if ret != 0 {
            return Err(RdmaError::Rdma("Failed to query GID".into()));
        }
        Ok(gid)
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        if self.owned && !self.context.is_null() {
            unsafe { ibv_close_device(self.context) };
        }
    }
}

// Sending Device across threads is generally safe if underlying rdma-core is thread safe (it usually is)
unsafe impl Send for Device {}
unsafe impl Sync for Device {}
