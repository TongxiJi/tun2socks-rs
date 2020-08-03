use std::collections::VecDeque;
use std::io;
use std::net::{Ipv4Addr, Shutdown, SocketAddr, SocketAddrV4};
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use futures::task::{Context, Poll, Waker};
use log::{debug, error, info, warn};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::ToSocketAddrs;
use tokio::net::{TcpStream, UdpSocket};

pub struct RedirectStream<T> {
    stream: T,
}

impl RedirectStream<TcpStream> {
    pub async fn new<A: ToSocketAddrs>(target: A) -> tokio::io::Result<Self> {
        let stream = TcpStream::connect(target).await?;
        Ok(RedirectStream { stream: stream })
    }
}

// unsafe impl Sync for RedirectStream<TcpStream> {}
// unsafe impl Send for RedirectStream<TcpStream> {}

impl<T> AsyncRead for RedirectStream<T>
where
    T: AsyncRead + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        AsyncRead::poll_read(Pin::new(&mut self.stream), cx, buf)
    }
}

impl<T> AsyncWrite for RedirectStream<T>
where
    T: AsyncWrite + Unpin,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        AsyncWrite::poll_write(Pin::new(&mut self.stream), cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        AsyncWrite::poll_flush(Pin::new(&mut self.stream), cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        AsyncWrite::poll_shutdown(Pin::new(&mut self.stream), cx)
    }
}

pub struct RedirectDatagram {
    socket: UdpSocket,
    redir_addr: SocketAddrV4,
}

impl RedirectDatagram {
    pub async fn new(redir_addr: SocketAddrV4) -> tokio::io::Result<Self> {
        let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::new(0, 0, 0, 0), 0)).await?;
        Ok(RedirectDatagram {
            socket: socket,
            redir_addr: redir_addr,
        })
    }

    async fn send_to<A: ToSocketAddrs>(
        &mut self,
        buf: &[u8],
        target: A,
    ) -> tokio::io::Result<usize> {
        self.socket.send_to(buf, self.redir_addr).await
    }
}
