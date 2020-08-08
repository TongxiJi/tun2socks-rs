use std::ffi;
use std::os::raw;
use std::ptr;

use crate::*;
use lwip::*;

pub fn ilog(msg: String) {
    unsafe {
        asl_log(
            ptr::null_mut(),
            ptr::null_mut(),
            ASL_LEVEL_NOTICE as i32,
            ffi::CString::new("%s").unwrap().as_c_str().as_ptr(),
            ffi::CString::new(msg.as_bytes())
                .unwrap()
                .as_c_str()
                .as_ptr(),
        )
    };
}
