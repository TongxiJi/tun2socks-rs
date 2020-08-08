use std::io::prelude::*;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use clap::{App, Arg};
use env_logger;
use futures::join;
use futures::prelude::*;
use futures::sink::SinkExt;
use futures::stream::StreamExt;
use log::{debug, error, info, warn};
use tokio;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::runtime;
use tun::{self, TunPacket};

use core;
use core::ilog;

pub const MTU: usize = 1500;

#[no_mangle]
pub extern "C" fn run(fd: std::os::raw::c_int) {
    ilog("run tun2socks".to_string());
    env_logger::init();

    let mut rt = runtime::Builder::new()
        .basic_scheduler()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let mut cfg = tun::Configuration::default();
        cfg.raw_fd(fd);
        let mut tun = tun::create_as_async(&cfg).unwrap();
        let mut framed = tun.into_framed();
        let (mut tun_sink, mut tun_stream) = framed.split();
        ilog("tun created".to_string());

        let proxy_addr = SocketAddr::from(SocketAddrV4::from_str("10.0.0.104:1080").unwrap());
        let mut stack = core::NetStack::new(proxy_addr);
        let mut stack = core::EventedNetStack::new(stack);
        let (mut stack_reader, mut stack_writer) = tokio::io::split(stack);
        ilog("tcp/ip stack created".to_string());

        let s2t = async move {
            let mut buf = [0; MTU];
            loop {
                match stack_reader.read(&mut buf).await {
                    Ok(0) => {
                        ilog(format!("read stack eof"));
                        return;
                    }
                    Ok(n) => {
                        tun_sink
                            .send(TunPacket::new((&buf[..n]).to_vec()))
                            .await
                            .unwrap();
                    }
                    Err(err) => {
                        ilog(format!("read stack failed {:?}", err));
                        return;
                    }
                }
            }
        };
        let t2s = async move {
            while let Some(packet) = tun_stream.next().await {
                match packet {
                    Ok(packet) => {
                        stack_writer.write(packet.get_bytes()).await.unwrap();
                    }
                    Err(err) => {
                        ilog(format!("read tun failed {:?}", err));
                        return;
                    }
                }
            }
        };

        tokio::select! {
            _ = s2t => ilog(format!("stack to tun ended")),
            _ = t2s => ilog(format!("tun to stack ended")),
        }
    });
}
