use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi;
use std::io;
use std::mem;
use std::net::ToSocketAddrs;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::ops::Drop;
use std::os::raw;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::time;

use futures::future::Future;
use futures::task::{Context, Poll, Waker};
use log::{debug, error, info, warn};
use tokio;
use tokio::stream::{Stream, StreamExt};
use tokio::sync::mpsc::{self, Sender};

use crate::*;
use lwip::*;
use util::*;

#[derive(Debug)]
pub struct UdpPacket {
    pub data: Box<[u8]>,
    pub addr: SocketAddr,
}

#[derive(Debug)]
pub struct RecvItem {
    pub lwip_lock: Arc<AtomicBool>,
    pub map: UdpMap,
    pub pcb: *mut udp_pcb,
    pub data: Box<[u8]>,
    pub src_addr: SocketAddr,
    pub dest_addr: SocketAddr,
}

unsafe impl Sync for RecvItem {}
unsafe impl Send for RecvItem {}

pub extern "C" fn udp_recv_cb(
    arg: *mut raw::c_void,
    pcb: *mut udp_pcb,
    p: *mut pbuf,
    addr: *const ip_addr_t,
    port: u16_t,
    dest_addr: *const ip_addr_t,
    dest_port: u16_t,
) {
    let mut listener = unsafe { &mut *(arg as *mut UdpListener) };
    let src_addr = unsafe { to_socket_addr(&*addr, port) };
    let dest_addr = unsafe { to_socket_addr(&*dest_addr, dest_port) };

    let tot_len = unsafe { (*p).tot_len };
    let n = tot_len as usize;
    let mut buf = Vec::<u8>::with_capacity(n);
    unsafe {
        pbuf_copy_partial(p, buf.as_mut_ptr() as *mut raw::c_void, tot_len, 0);
        buf.set_len(n);
        pbuf_free(p);
    }
    let boxed_data = (&buf[..n]).to_vec().into_boxed_slice();

    let mut map = Arc::clone(&listener.udp_map);

    match listener.queue.lock() {
        Ok(mut queue) => {
            let ri = Box::new(RecvItem {
                lwip_lock: listener.lwip_lock.clone(),
                map: map,
                pcb: pcb,
                data: boxed_data,
                src_addr: src_addr,
                dest_addr: dest_addr,
            });
            queue.push_back(ri);
            match listener.waker.lock() {
                Ok(mut waker) => {
                    if let Some(mut waker) = waker.take() {
                        waker.wake();
                    }
                }
                Err(err) => {
                    error!("udp waker lock waker failed {:?}", err);
                }
            }
        }
        Err(err) => {
            error!("udp listener lock queue failed {:?}", err);
        }
    }
}

pub type UdpMap = Arc<tokio::sync::Mutex<HashMap<SocketAddr, mpsc::Sender<UdpPacket>>>>;

pub struct UdpListener {
    lwip_lock: Arc<AtomicBool>,
    waker: Arc<Mutex<Option<Waker>>>,
    queue: Arc<Mutex<VecDeque<Box<RecvItem>>>>,
    udp_map: UdpMap,
}

impl UdpListener {
    pub fn new(lwip_lock: Arc<AtomicBool>) -> Self {
        let listener = UdpListener {
            lwip_lock: lwip_lock,
            waker: Arc::new(Mutex::new(None)),
            queue: Arc::new(Mutex::new(VecDeque::new())),
            udp_map: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        };
        unsafe {
            let mut upcb = udp_new();
            let err = udp_bind(upcb, &ip_addr_any_type, 0);
            if err != err_enum_t_ERR_OK as err_t {
                panic!("bind udp failed");
            }
            let arg = &listener as *const UdpListener as *mut raw::c_void;
            udp_recv(upcb, Some(udp_recv_cb), arg);
        }
        listener
    }
}

impl Stream for UdpListener {
    type Item = Box<RecvItem>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        match self.queue.lock() {
            Ok(mut queue) => {
                if let Some(sess) = queue.pop_front() {
                    return Poll::Ready(Some(sess));
                }
            }
            Err(err) => {
                error!("sess poll lock queue failed: {:?}", err);
            }
        }
        match self.waker.lock() {
            Ok(mut waker) => {
                if waker.is_none() {
                    waker.replace(cx.waker().clone());
                }
            }
            Err(err) => {
                error!("sess poll lock waker failed: {:?}", err);
            }
        }
        Poll::Pending
    }
}
