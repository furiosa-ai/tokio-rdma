use std::ffi::CStr;

/// Converts a raw C string pointer to a Rust String.
///
/// # Safety
///
/// The caller must ensure that `ptr` is a valid, null-terminated C string pointer.
pub unsafe fn ptr_to_string(ptr: *const i8) -> String {
    if ptr.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(ptr).to_string_lossy().into_owned() }
}
