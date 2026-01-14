use std::ffi::CStr;

pub unsafe fn ptr_to_string(ptr: *const i8) -> String {
    if ptr.is_null() {
        return String::new();
    }
    CStr::from_ptr(ptr).to_string_lossy().into_owned()
}
