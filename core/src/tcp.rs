use std::collections::VecDeque;
use std::ffi;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, SocketAddrV4};
use std::os::raw;
use std::pin::Pin;
use std::ptr;
use std::rc::Rc;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, sync_channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;

use futures::task::{Context, Poll, Waker};
use log::{debug, error, info, trace, warn};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::stream::{Stream, StreamExt};

use crate::*;
use lwip::*;
use stack::*;
use util::*;

pub extern "C" fn tcp_accept_cb(arg: *mut raw::c_void, newpcb: *mut tcp_pcb, err: err_t) -> err_t {
    let mut listener = unsafe { &mut *(arg as *mut TcpListener) };
    listener
        .queue
        .push_back(TcpStream::new(listener.lwip_lock.clone(), newpcb));
    if let Some(waker) = listener.waker.take() {
        waker.wake();
    }
    err_enum_t_ERR_OK as err_t
}

pub extern "C" fn tcp_recv_cb(
    arg: *mut raw::c_void,
    tpcb: *mut tcp_pcb,
    p: *mut pbuf,
    err: err_t,
) -> err_t {
    unsafe {
        let mut stream: &mut TcpStream;
        let mut buf: Vec<u8>;
        let mut buflen: usize = 0;

        stream = &mut *(arg as *mut TcpStream);

        if p.is_null() {
            let mut buf = [0; 0];
            stream.tx.send((&buf[..]).to_vec().into_boxed_slice());
            if let Some(waker) = stream.waker.take() {
                waker.wake();
            }
            return err_enum_t_ERR_OK as err_t;
        }

        let pbuflen = (*p).tot_len;
        buflen = pbuflen as usize;
        buf = Vec::<u8>::with_capacity(buflen);
        pbuf_copy_partial(p, buf.as_mut_ptr() as *mut raw::c_void, pbuflen, 0);
        buf.set_len(pbuflen as usize);

        stream.tx.send((&buf[..buflen]).to_vec().into_boxed_slice());

        if let Some(waker) = stream.waker.take() {
            waker.wake();
        }
        err_enum_t_ERR_OK as err_t
    }
}

pub extern "C" fn tcp_sent_cb(arg: *mut raw::c_void, tpcb: *mut tcp_pcb, len: u16_t) -> err_t {
    unsafe {
        let mut stream: &mut TcpStream;
        stream = &mut *(arg as *mut TcpStream);
        if let Some(waker) = stream.write_waker.take() {
            waker.wake();
        }
        err_enum_t_ERR_OK as err_t
    }
}

pub extern "C" fn tcp_err_cb(arg: *mut ::std::os::raw::c_void, err: err_t) {
    unsafe {
        let mut stream: &mut TcpStream;
        stream = &mut *(arg as *mut TcpStream);
        debug!("tcp_err_cb");
        stream.errored = true;
    }
}

pub struct TcpListener {
    pub lwip_lock: Arc<AtomicBool>,
    pub waker: Option<Waker>,
    pub queue: VecDeque<Box<TcpStream>>,
}

impl TcpListener {
    pub fn new(lwip_lock: Arc<AtomicBool>) -> Self {
        let listener = TcpListener {
            lwip_lock: lwip_lock,
            waker: None,
            queue: VecDeque::new(),
        };
        unsafe {
            while listener
                .lwip_lock
                .compare_and_swap(false, true, Ordering::Acquire)
            {}

            let mut tpcb = tcp_new();
            let err = tcp_bind(tpcb, &ip_addr_any_type, 0);
            if err != err_enum_t_ERR_OK as err_t {
                panic!("bind tcp failed");
            }
            tpcb = tcp_listen_with_backlog(tpcb, TCP_DEFAULT_LISTEN_BACKLOG as u8);
            if tpcb.is_null() {
                panic!("listen tcp failed");
            }
            let arg = &listener as *const TcpListener as *mut raw::c_void;
            tcp_arg(tpcb, arg);
            tcp_accept(tpcb, Some(tcp_accept_cb));

            listener.lwip_lock.store(false, Ordering::Release);
        }
        listener
    }
}

impl Stream for TcpListener {
    type Item = Box<TcpStream>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        if let Some(stream) = self.queue.pop_front() {
            return Poll::Ready(Some(stream));
        }
        if self.waker.is_none() {
            self.waker.replace(cx.waker().clone());
        }
        Poll::Pending
    }
}

pub struct TcpStream {
    lwip_lock: Arc<AtomicBool>,
    src_addr: SocketAddr,
    dest_addr: SocketAddr,
    pcb: *mut tcp_pcb,
    waker: Option<Waker>,
    write_waker: Option<Waker>,
    tx: Sender<Box<[u8]>>,
    rx: Receiver<Box<[u8]>>,
    errored: bool,
}

impl TcpStream {
    pub fn new(lwip_lock: Arc<AtomicBool>, pcb: *mut tcp_pcb) -> Box<Self> {
        unsafe {
            let (tx, rx): (Sender<Box<[u8]>>, Receiver<Box<[u8]>>) = mpsc::channel();
            let src_addr = to_socket_addr(&(*pcb).remote_ip, (*pcb).remote_port);
            let dest_addr = to_socket_addr(&(*pcb).local_ip, (*pcb).local_port);
            let stream = Box::new(TcpStream {
                lwip_lock: lwip_lock,
                src_addr,
                dest_addr,
                pcb: pcb,
                waker: None,
                write_waker: None,
                tx: tx,
                rx: rx,
                errored: false,
            });
            let arg = &*stream as *const TcpStream as *mut raw::c_void;
            tcp_arg(pcb, arg);
            tcp_recv(pcb, Some(tcp_recv_cb));
            tcp_sent(pcb, Some(tcp_sent_cb));
            tcp_err(pcb, Some(tcp_err_cb));
            stream
        }
    }

    pub fn local_addr(&self) -> &SocketAddr {
        return &self.src_addr;
    }

    pub fn remote_addr(&self) -> &SocketAddr {
        return &self.dest_addr;
    }
}

impl Drop for TcpStream {
    fn drop(&mut self) {
        unsafe {
            while self
                .lwip_lock
                .compare_and_swap(false, true, Ordering::Acquire)
            {}

            if !self.errored {
                tcp_arg(self.pcb, std::ptr::null_mut());
                tcp_recv(self.pcb, None);
                tcp_sent(self.pcb, None);
                tcp_err(self.pcb, None);
                tcp_close(self.pcb);
            }

            self.lwip_lock.store(false, Ordering::Release);
        }
    }
}

impl AsyncRead for TcpStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        match self.rx.try_recv() {
            Ok(mut data) => {
                let data = data.as_ref();
                (&mut buf[..data.len()]).clone_from_slice(data);
                unsafe {
                    while self
                        .lwip_lock
                        .compare_and_swap(false, true, Ordering::Acquire)
                    {}

                    tcp_recved(self.pcb, data.len() as u16_t);

                    self.lwip_lock.store(false, Ordering::Release);
                }
                return Poll::Ready(Ok(data.len()));
            }
            Err(err) => {
                if self.waker.is_none() {
                    self.waker.replace(cx.waker().clone());
                }
                Poll::Pending
            }
        }
    }
}

unsafe impl Sync for TcpStream {}
unsafe impl Send for TcpStream {}

impl AsyncWrite for TcpStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let mut to_write = buf.len();

        while self
            .lwip_lock
            .compare_and_swap(false, true, Ordering::Acquire)
        {}

        unsafe {
            let snd_buf_size = (*self.pcb).snd_buf as usize;
            if snd_buf_size < to_write {
                to_write = snd_buf_size;
            }
            if to_write == 0 {
                if self.write_waker.is_none() {
                    self.write_waker.replace(cx.waker().clone());
                }

                self.lwip_lock.store(false, Ordering::Release);

                return Poll::Pending;
            }
            let err = tcp_write(
                self.pcb,
                buf.as_ptr() as *const raw::c_void,
                to_write as u16_t,
                TCP_WRITE_FLAG_COPY as u8,
            );
            if err == err_enum_t_ERR_OK as err_t {
                tcp_output(self.pcb);
            } else if err == err_enum_t_ERR_MEM as err_t {
                debug!("err_mem tcp_output");
                tcp_output(self.pcb);
            } else {
                self.lwip_lock.store(false, Ordering::Release);

                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    format!("tcp_write error {:?}", err),
                )));
            }
        }

        self.lwip_lock.store(false, Ordering::Release);

        return Poll::Ready(Ok(to_write));
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        // FIXME perhaps call tcp_output?
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

pub struct EventedTcpStream {
    inner: Box<TcpStream>,
}

impl EventedTcpStream {
    pub fn new(stream: Box<TcpStream>) -> Self {
        EventedTcpStream { inner: stream }
    }
}

impl AsyncRead for EventedTcpStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        AsyncRead::poll_read(Pin::new(&mut self.inner), cx, buf)
    }
}

impl AsyncWrite for EventedTcpStream {
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
