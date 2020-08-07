use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::os::raw;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Once};
use std::thread;
use std::time;

use async_socks5;
use async_socks5::{connect, AddrKind, Auth, SocksDatagram};
use futures::future::{self, FutureExt, MapErr};
use futures::prelude::*;
use futures::stream::{ForEach, Stream, StreamExt};
use futures::task::{Context, Poll, Waker};
use log::{debug, error, info, warn};
use std::sync::mpsc::{self, sync_channel, Receiver, Sender};
use tokio;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpStream, ToSocketAddrs, UdpSocket};

use crate::*;
use lwip::*;
use output::*;
use tcp::*;
use udp::*;
use util::*;

static LWIP_INIT: Once = Once::new();

pub struct NetStack {
    pub lwip_lock: Arc<AtomicBool>,
    waker: Arc<Mutex<Option<Waker>>>,
    tx: Sender<Box<[u8]>>,
    rx: Receiver<Box<[u8]>>,
}

unsafe impl Sync for NetStack {}
unsafe impl Send for NetStack {}

impl NetStack {
    pub fn new(proxy_addr: SocketAddr) -> Box<Self> {
        LWIP_INIT.call_once(|| unsafe { lwip_init() });

        unsafe {
            (*netif_list).output = Some(output_ip4);
            (*netif_list).mtu = 1500;
            // (*netif_list).output_ip6 = Some(output_ip6);
        }

        let (tx, rx): (Sender<Box<[u8]>>, Receiver<Box<[u8]>>) = mpsc::channel();
        let stack = Box::new(NetStack {
            lwip_lock: Arc::new(AtomicBool::new(false)),
            waker: Arc::new(Mutex::new(None)),
            tx: tx,
            rx: rx,
        });

        unsafe {
            let arg = &*stack as *const NetStack as *mut raw::c_void;
            OUTPUT_CB_PTR = &*stack as *const NetStack as usize;
        }

        let lwip_lock = stack.lwip_lock.clone();
        tokio::spawn(async move {
            loop {
                {
                    // TODO build our own mutex using atomic type
                    while lwip_lock.compare_and_swap(false, true, Ordering::Acquire) {}

                    unsafe { sys_check_timeouts() };

                    lwip_lock.store(false, Ordering::Release);
                }
                tokio::time::delay_for(time::Duration::from_millis(250)).await;
            }
        });

        let lwip_locktcp = stack.lwip_lock.clone();
        tokio::spawn(async move {
            let mut listener = TcpListener::new(lwip_locktcp);

            while let Some(mut stream) = listener.next().await {
                info!(
                    "tcp {}:{} -> {}:{}",
                    stream.local_addr().ip(),
                    stream.local_addr().port(),
                    stream.remote_addr().ip(),
                    stream.remote_addr().port()
                );
                tokio::spawn(async move {
                    match TcpStream::connect(proxy_addr).await {
                        Ok(mut socks_stream) => {
                            if let Err(err) =
                                connect(&mut socks_stream, stream.remote_addr().clone(), None).await
                            {
                                warn!("socks5 connect failed {:?}", err);
                                return;
                            }
                            let mut stream = EventedTcpStream::new(stream);

                            let (mut lr, mut lw) = tokio::io::split(stream);
                            let (mut rr, mut rw) = tokio::io::split(socks_stream);
                            let mut buf_lr = tokio::io::BufReader::with_capacity(32 * 1024, lr);

                            let r2l = tokio::io::copy(&mut rr, &mut lw);
                            let l2r = tokio::io::copy(&mut buf_lr, &mut rw);

                            tokio::select! {
                                r1 = r2l => debug!("r2l done {:?}", r1),
                                r2 = l2r => debug!("l2r done {:?}", r2),
                            }
                        }
                        Err(err) => {
                            warn!("connect proxy failed {:?}", err);
                            return;
                        }
                    }
                });
            }
        });

        let lwip_lock_udp = stack.lwip_lock.clone();
        tokio::spawn(async move {
            let mut listener = UdpListener::new(lwip_lock_udp);
            while let Some(mut ri) = listener.next().await {
                let src_addr = ri.src_addr.clone();
                let map = ri.map.clone();
                if !map.lock().await.contains_key(&src_addr) {
                    info!(
                        "udp {}:{} -> {}:{}",
                        ri.src_addr.ip(),
                        ri.src_addr.port(),
                        ri.dest_addr.ip(),
                        ri.dest_addr.port()
                    );

                    let any_addr = SocketAddrV4::new(Ipv4Addr::new(0, 0, 0, 0), 0);
                    let stream = match TcpStream::connect(proxy_addr).await {
                        Ok(stream) => stream,
                        Err(err) => {
                            warn!("connect proxy failed {:?}", err);
                            continue;
                        }
                    };
                    let mut socket = match UdpSocket::bind("0.0.0.0:0").await {
                        Ok(mut socket) => socket,
                        Err(err) => {
                            warn!("bind udp failed: {:?}", err);
                            continue;
                        }
                    };
                    let mut dg = match SocksDatagram::associate(
                        stream,
                        socket,
                        None::<Auth>,
                        None::<AddrKind>,
                    )
                    .await
                    {
                        Ok(mut dg) => dg,
                        Err(err) => {
                            warn!("socks5 udp associate failed: {:?}", err);
                            continue;
                        }
                    };

                    let (mut recv_half, mut send_half) = dg.split();

                    let (mut u_tx, mut u_rx) = tokio::sync::mpsc::channel(100);

                    map.lock().await.insert(src_addr, u_tx);

                    // FIXME update the timer upon receiving new pkts on this session
                    use futures::future::{AbortHandle, Abortable, Aborted};
                    let map = ri.map.clone();
                    let src_addr = ri.src_addr.clone();
                    let (downlink_abort_handle, downlink_abort_registration) =
                        AbortHandle::new_pair();
                    let (uplink_abort_handle, uplink_abort_registration) = AbortHandle::new_pair();
                    let downlink_timeout_task = async move {
                        tokio::time::delay_for(time::Duration::from_secs(60)).await;
                        downlink_abort_handle.abort();
                        map.lock().await.remove(&src_addr);
                    };
                    let uplink_timeout_task = async move {
                        tokio::time::delay_for(time::Duration::from_secs(60)).await;
                        uplink_abort_handle.abort();
                    };
                    tokio::spawn(downlink_timeout_task);
                    tokio::spawn(uplink_timeout_task);

                    let lwip_lock = ri.lwip_lock.clone();
                    let src_addr = ri.src_addr.clone();
                    let dest_addr = ri.dest_addr.clone();
                    let pcb = ri.pcb as usize;

                    // downlink, receive from remote then send to local
                    let downlink_task = async move {
                        let mut buf = [0; 2 * 1024];
                        loop {
                            match recv_half.recv_from(&mut buf).await {
                                Ok((0, _)) => {
                                    warn!("recv_from remote end");
                                    break;
                                }
                                Ok((n, addr)) => unsafe {
                                    while lwip_lock.compare_and_swap(false, true, Ordering::Acquire)
                                    {
                                    }

                                    let pbuf = pbuf_alloc_reference(
                                        &buf[..n] as *const [u8] as *mut [u8] as *mut raw::c_void,
                                        n as u16_t,
                                        pbuf_type_PBUF_ROM,
                                    );
                                    let src_ip = to_ip_addr_t(&src_addr.ip()).unwrap();
                                    let dest_ip = to_ip_addr_t(&dest_addr.ip()).unwrap();
                                    udp_sendto(
                                        pcb as *mut udp_pcb,
                                        pbuf,
                                        &src_ip as *const ip_addr_t,
                                        src_addr.port() as u16_t,
                                        &dest_ip as *const ip_addr_t,
                                        dest_addr.port() as u16_t,
                                    );
                                    pbuf_free(pbuf);

                                    lwip_lock.store(false, Ordering::Release);
                                },
                                Err(err) => {
                                    warn!("recv_from remote {:?}", err);
                                    break;
                                }
                            }
                        }
                    };
                    let downlink_task = Abortable::new(downlink_task, downlink_abort_registration);
                    tokio::spawn(downlink_task);

                    // uplink, receive from local then send to remote
                    let uplink_task = async move {
                        while let Some(pkt) = u_rx.recv().await {
                            match send_half.send_to(pkt.data.as_ref(), pkt.addr).await {
                                Ok(0) => {
                                    warn!("send to remote 0 len");
                                    break;
                                }
                                Ok(n) => {}
                                Err(err) => {
                                    warn!("send to remote {:?}", err);
                                    break;
                                }
                            }
                        }
                    };
                    let uplink_task = Abortable::new(uplink_task, uplink_abort_registration);
                    tokio::spawn(uplink_task);
                }

                let mut map = map.lock().await;
                let mut u_tx = map.get_mut(&ri.src_addr).unwrap();
                u_tx.send(UdpPacket {
                    data: ri.data,
                    addr: ri.dest_addr,
                })
                .await;
            }
        });

        stack
    }

    pub fn output(&mut self, pkt: Box<[u8]>) -> io::Result<usize> {
        let n = pkt.len();
        self.tx.send(pkt);
        if let Some(mut waker) = self.waker.lock().unwrap().take() {
            waker.wake();
            return Ok(n);
        }
        Ok(0)
    }
}

impl AsyncRead for NetStack {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        match self.rx.try_recv() {
            Ok(mut pkt) => {
                let pkt = pkt.as_ref();
                (&mut buf[..pkt.len()]).clone_from_slice(pkt);
                return Poll::Ready(Ok(pkt.len()));
            }
            Err(err) => {
                if let Ok(mut waker) = self.waker.lock() {
                    if waker.is_none() {
                        waker.replace(cx.waker().clone());
                    }
                } else {
                    error!("lock stack waker failed");
                }
                Poll::Pending
            }
        }
    }
}

impl AsyncWrite for NetStack {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        unsafe {
            let pbuf = pbuf_alloc(pbuf_layer_PBUF_RAW, buf.len() as u16_t, pbuf_type_PBUF_POOL);
            pbuf_take(pbuf, buf.as_ptr() as *const raw::c_void, buf.len() as u16_t);

            while self
                .lwip_lock
                .compare_and_swap(false, true, Ordering::Acquire)
            {}

            if let Some(input_fn) = (*netif_list).input {
                let err = input_fn(pbuf, netif_list);
                if err == err_enum_t_ERR_OK as err_t {
                    self.lwip_lock.store(false, Ordering::Release);
                    return Poll::Ready(Ok(buf.len()));
                } else {
                    pbuf_free(pbuf);
                    self.lwip_lock.store(false, Ordering::Release);
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "input failed",
                    )));
                }
            } else {
                pbuf_free(pbuf);
                self.lwip_lock.store(false, Ordering::Release);
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "none input fn",
                )));
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

pub struct EventedNetStack {
    inner: Box<NetStack>,
}

impl EventedNetStack {
    pub fn new(stack: Box<NetStack>) -> Self {
        EventedNetStack { inner: stack }
    }
}

impl AsyncRead for EventedNetStack {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        AsyncRead::poll_read(Pin::new(&mut self.inner), cx, buf)
    }
}

impl AsyncWrite for EventedNetStack {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        AsyncWrite::poll_write(Pin::new(&mut self.inner), cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        AsyncWrite::poll_flush(Pin::new(&mut self.inner), cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        AsyncWrite::poll_shutdown(Pin::new(&mut self.inner), cx)
    }
}
