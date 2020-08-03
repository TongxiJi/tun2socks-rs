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

const MTU: usize = 1500;

#[cfg(target_os = "macos")]
fn default_tun_name() -> &'static str {
    "utun8"
}

#[cfg(target_os = "linux")]
fn default_tun_name() -> &'static str {
    "tun8"
}

fn main() {
    let matches = App::new("tun2socks")
        .version("0.1")
        .about("Turing TCP and UDP traffic into SOCKS connections.")
        .arg(
            Arg::with_name("tun-name")
                .long("tun-name")
                .value_name("NAME")
                .about("Set the TUN interface name")
                .takes_value(true)
                .default_value(default_tun_name()),
        )
        .arg(
            Arg::with_name("tun-addr")
                .long("tun-addr")
                .value_name("IP")
                .about("Set the IP address of the TUN interface")
                .takes_value(true)
                .default_value("10.10.0.2"),
        )
        .arg(
            Arg::with_name("tun-mask")
                .long("tun-mask")
                .value_name("MASK")
                .about("Set the network mask of the TUN interface")
                .takes_value(true)
                .default_value("255.255.255.0"),
        )
        .arg(
            Arg::with_name("tun-gw")
                .long("tun-gw")
                .value_name("GATEWAY")
                .about("Set the gateway address of the TUN interface")
                .takes_value(true)
                .default_value("10.10.0.1"),
        )
        .arg(
            Arg::with_name("proxy-server")
                .long("proxy-server")
                .value_name("IP:PORT")
                .about("Set the SOCKS5 proxy server address")
                .takes_value(true)
                .required(true),
        )
        .get_matches();
    let tun_name = matches.value_of("tun-name").unwrap();
    let tun_addr = matches.value_of("tun-addr").unwrap();
    let tun_mask = matches.value_of("tun-mask").unwrap();
    let tun_gw = matches.value_of("tun-gw").unwrap();
    let proxy_server = matches.value_of("proxy-server").unwrap();

    env_logger::init();

    //let mut rt = runtime::Builder::new()
    //    .threaded_scheduler()
    //    .core_threads(2)
    //    .thread_stack_size(1 * 1024 * 1024)
    //    .enable_all()
    //    .build()
    //    .unwrap();

    let mut rt = runtime::Builder::new()
        .basic_scheduler()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let mut cfg = tun::Configuration::default();
        cfg.name(tun_name)
            .address(tun_addr)
            .netmask(tun_mask)
            .destination(tun_gw)
            .mtu(MTU as i32)
            .up();

        #[cfg(target_os = "linux")]
        cfg.platform(|cfg| {
            cfg.packet_information(true);
        });

        let mut tun = tun::create_as_async(&cfg).unwrap();
        let mut framed = tun.into_framed();
        let (mut tun_sink, mut tun_stream) = framed.split();
        info!("TUN created.");

        let proxy_addr = SocketAddr::from(SocketAddrV4::from_str(proxy_server).unwrap());
        let mut stack = core::NetStack::new(proxy_addr);
        let mut stack = core::EventedNetStack::new(stack);
        let (mut stack_reader, mut stack_writer) = tokio::io::split(stack);
        info!("TCP/IP stack created.");

        let s2t = async move {
            let mut buf = [0; MTU];
            loop {
                match stack_reader.read(&mut buf).await {
                    Ok(0) => {
                        error!("read stack eof");
                        return;
                    }
                    Ok(n) => {
                        tun_sink
                            .send(TunPacket::new((&buf[..n]).to_vec()))
                            .await
                            .unwrap();
                    }
                    Err(err) => {
                        error!("read stack failed {:?}", err);
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
                        error!("read tun failed {:?}", err);
                        return;
                    }
                }
            }
        };

        tokio::select! {
            _ = s2t => warn!("stack to tun ended"),
            _ = t2s => warn!("tun to stack ended"),
        }
    });
}
