use std::os::raw;

use crate::*;
use lwip::*;
use stack::*;

pub static mut OUTPUT_CB_PTR: usize = 0x0;

fn output(netif: *mut netif, p: *mut pbuf) -> err_t {
    unsafe {
        let pbuflen = (*p).tot_len;
        let mut buf = vec![0; 1500];
        pbuf_copy_partial(p, buf.as_mut_ptr() as *mut raw::c_void, pbuflen, 0);
        buf.set_len(pbuflen as usize);
        let stack = &mut *(OUTPUT_CB_PTR as *mut NetStack);
        stack.output((&buf[0..pbuflen as usize]).to_vec().into_boxed_slice());
        err_enum_t_ERR_OK as err_t
    }
}

pub extern "C" fn output_ip4(netif: *mut netif, p: *mut pbuf, ipaddr: *const ip4_addr_t) -> err_t {
    output(netif, p)
}

pub extern "C" fn output_ip6(netif: *mut netif, p: *mut pbuf, ipaddr: *const ip6_addr_t) -> err_t {
    output(netif, p)
}
