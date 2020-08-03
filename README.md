# tun2socks-rs

A tun2socks implementation written in Rust.

## Prerequisites

https://rust-lang.github.io/rust-bindgen/requirements.html

### macOS

```sh
brew install llvm
```

### Debian

```sh
apt install llvm-dev libclang-dev clang
```

## Build

```sh
git clone --recurse-submodules https://github.com/eycorsican/tun2socks-rs.git
cd tun2socks-rs
cargo build
```

## Run

```sh
sudo RUST_LOG=debug ./target/debug/tun2socks --proxy-server 1.2.3.4:1080
```

### macOS

```sh
sudo ifconfig utun7 10.10.0.2 netmask 255.255.255.0 10.10.0.1
sudo route delete default
sudo route add default 10.10.0.1
```

### Linux

```sh
sudo ip route del default
sudo ip route add default via 10.10.0.1
```
