use std::ffi;
use std::mem;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::str::FromStr;

use crate::*;
use lwip::*;

pub fn to_socket_addr(addr: &ip_addr_t, port: u16_t) -> SocketAddr {
    unsafe {
        let src_ip = ffi::CStr::from_ptr(ipaddr_ntoa(addr)).to_str().unwrap();
        SocketAddr::from(SocketAddrV4::new(
            Ipv4Addr::from_str(src_ip).unwrap(),
            port as u16,
        ))
    }
}

pub fn to_ip_addr_t(ip: &IpAddr) -> Result<ip_addr_t, &'static str> {
    unsafe {
        let mut ip_addr = ip_addr_t {
            u_addr: unsafe { mem::zeroed() },
            type_: unsafe { mem::zeroed() },
        };
        let addr_str = ip.to_string();
        let addr_str_bytes = addr_str.as_bytes();
        let addr_cstring = ffi::CString::new(addr_str_bytes).unwrap();
        let addr_cstring_bytes = addr_cstring.to_bytes_with_nul();
        let cp = ffi::CStr::from_bytes_with_nul_unchecked(addr_cstring_bytes).as_ptr();
        let ret = ipaddr_aton(cp, &mut ip_addr as *mut ip_addr_t);
        if ret == 0 {
            return Err("ipaddr_aton failed");
        }
        Ok(ip_addr)
    }
}
