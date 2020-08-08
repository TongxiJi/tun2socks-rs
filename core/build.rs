extern crate bindgen;
extern crate cc;

use std::env;
use std::path::PathBuf;

fn main() {
    cc::Build::new()
        .file("lwip/core/init.c")
        .file("lwip/core/def.c")
        .file("lwip/core/dns.c")
        .file("lwip/core/inet_chksum.c")
        .file("lwip/core/ip.c")
        .file("lwip/core/mem.c")
        .file("lwip/core/memp.c")
        .file("lwip/core/netif.c")
        .file("lwip/core/pbuf.c")
        .file("lwip/core/raw.c")
        .file("lwip/core/stats.c")
        .file("lwip/core/sys.c")
        .file("lwip/core/tcp.c")
        .file("lwip/core/tcp_in.c")
        .file("lwip/core/tcp_out.c")
        .file("lwip/core/timeouts.c")
        .file("lwip/core/udp.c")
        .file("lwip/core/ipv4/autoip.c")
        .file("lwip/core/ipv4/dhcp.c")
        .file("lwip/core/ipv4/etharp.c")
        .file("lwip/core/ipv4/icmp.c")
        .file("lwip/core/ipv4/igmp.c")
        .file("lwip/core/ipv4/ip4_frag.c")
        .file("lwip/core/ipv4/ip4.c")
        .file("lwip/core/ipv4/ip4_addr.c")
        .file("lwip/core/ipv6/dhcp6.c")
        .file("lwip/core/ipv6/ethip6.c")
        .file("lwip/core/ipv6/icmp6.c")
        .file("lwip/core/ipv6/inet6.c")
        .file("lwip/core/ipv6/ip6.c")
        .file("lwip/core/ipv6/ip6_addr.c")
        .file("lwip/core/ipv6/ip6_frag.c")
        .file("lwip/core/ipv6/mld6.c")
        .file("lwip/core/ipv6/nd6.c")
        .file("lwip/custom/sys_arch.c")
        .include("lwip/custom")
        .include("lwip/include")
        .compile("liblwip.a");

    println!("cargo:rustc-link-lib=lwip");
    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:include=lwip/include");

    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let bindings = bindgen::Builder::default()
        .header("wrapper.h")
        .clang_arg("-I./lwip/include")
        .clang_arg("-I./lwip/custom")
        .layout_tests(false)

        // https://github.com/rust-lang/rust-bindgen/issues/1211
        .clang_arg(if arch == "aarch64" && os == "ios" {
            "--target=arm64-apple-ios"
        } else {
            ""
        })
        .clang_arg(if arch == "aarch64" && os == "ios" {
            "-I/Applications/Xcode.app/Contents/Developer/Platforms/iPhoneOS.platform/Developer/SDKs/iPhoneOS.sdk/usr/include"
        } else {
            ""
        })

        .parse_callbacks(Box::new(bindgen::CargoCallbacks))
        .generate()
        .expect("Unable to generate bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings!");
}
