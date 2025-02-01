/*
TeleDDNS
(C) 2015-2025 Tomas Hlavacek (tmshlvck@gmail.com)

This program is free software: you can redistribute it and/or modify
it under the terms of the GNU General Public License as published by
the Free Software Foundation, either version 3 of the License, or
(at your option) any later version.

This program is distributed in the hope that it will be useful,
but WITHOUT ANY WARRANTY; without even the implied warranty of
MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
GNU General Public License for more details.

You should have received a copy of the GNU General Public License
along with this program.  If not, see <http://www.gnu.org/licenses/>.
*/

use log::{debug, info, warn, error};
use netlink_packet_route::address::AddressHeader;
use netlink_packet_route::AddressFamily;
use std::net::{IpAddr,Ipv4Addr,Ipv6Addr};
use tokio::task::JoinSet;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc::{self, Sender, Receiver};
use std::panic;
use futures::stream::{StreamExt, TryStreamExt};
use netlink_sys::{AsyncSocket, SocketAddr};
use rtnetlink::{new_connection, Error, Handle};
use netlink_packet_core::NetlinkPayload::InnerMessage;
use netlink_packet_route::link::*;
use netlink_packet_route::RouteNetlinkMessage::*;
use netlink_packet_route::address::*;
use std::time::SystemTime;
use std::fs::File;
use std::io::prelude::*;
use std::io::LineWriter;
use std::collections::HashMap;
use clap::Parser;
use serde_derive::{Serialize, Deserialize};
use tokio::time::{sleep, Duration};
use async_process::Command;

const MIN_UPDATE_INTERVAL_S: u64 = 30;

const RTNLGRP_LINK: u32 = 1;
const RTNLGRP_IPV4_IFADDR: u32 = 1 << 4;
const RTNLGRP_IPV6_IFADDR: u32 = 1 << 8;
const RTNLGRP_IPV4_NETCONF: u32 = 1 << 23;
const RTNLGRP_IPV6_NETCONF: u32 = 1 << 24;


async fn nl_get_addrs(handle: &Handle) -> Result<Vec<(IpAddr, AddressHeader)>, Error> {
    let mut aget = handle
        .address()
        .get()
        .execute();
    let mut res = Vec::new();
    while let Some(msg) = aget.try_next().await? {
        debug!("response: {:?}", msg);
        for attr in msg.attributes.into_iter() {
            if let AddressAttribute::Address(addr) = attr {
                res.push((addr, msg.header.clone()));
            }
        }
    }
    Ok(res)
}

async fn nl_get_iface_map(handle: &Handle) -> Result<HashMap<u32, String>, Error> {
    let mut lget = handle.link().get().execute();
    let mut res: HashMap<u32, String> = HashMap::new();
    while let Some(msg) = lget.try_next().await? {
        debug!("response {:?}", msg);
        let mut ifname: Option<String> = None;
        let mut is_up = false;
        if msg.header.flags.contains(&LinkFlag::Up) &&
            msg.header.flags.contains(&LinkFlag::LowerUp) &&
            msg.header.flags.contains(&LinkFlag::Running) {
            is_up = true;
        }
        for nla in msg.attributes.into_iter() {
            match nla {
                LinkAttribute::IfName(ifn) => {
                    ifname = Some(ifn);
                },
                LinkAttribute::OperState(State::Up) => {
                    is_up = true;
                },
                _ => {}
            }
        }
        if ifname.is_some() && is_up {
            res.insert(msg.header.index, ifname.unwrap());
        }
    }
    Ok(res)
}

fn match_iface_pattern(ifname: &str, pattern_list: &Vec<String>) -> bool {
    let negative = format!("-{}", ifname);
    for ptr in pattern_list {
        if ptr == &negative{
            return false;
        }
        if ptr == ifname {
            return true;
        }
        if ptr == "*" {
            return true;
        }
    }
    false
}

fn iface_bonus(iface: &str) -> u8 {
    if iface.starts_with("en") {
        2
    } else if iface.starts_with("wl") {
        1
    } else {
        0
    }
}

fn flag_bonus(flags: &Vec<AddressHeaderFlag>) -> u8 {
    /*let mut b = 0;
    if flags.contains(&AddressHeaderFlag::Permanent) {
        b += 1;
    }
    b*/
    0
}

fn compute_v6_metric(ipaddr: &Ipv6Addr, iface: &str, flags: &Vec<AddressHeaderFlag>, accept_ula: bool) -> u8 {
    if ipaddr.is_unicast_link_local() || ipaddr.is_multicast() {
        0
    } else if matches!(ipaddr.segments(), [0, 0, 0, 0, 0, 0xffff, _, _]) { // .is_ipv4_mapped
        0
    } else if ipaddr.is_loopback() || ipaddr.is_unspecified() {
        0
    } else if matches!(ipaddr.segments(), [0x2001, 0xdb8, ..] | [0x3fff, 0..=0x0fff, ..]) { // .is_documentation
        0
    } else if (ipaddr.segments()[0] == 0x2001) && (ipaddr.segments()[1] == 0x2) && (ipaddr.segments()[2] == 0) { // .is_benchmarking
        0
    } else if ipaddr.is_unique_local() {
        if accept_ula {
            2 + flag_bonus(flags) + iface_bonus(iface)
        } else {
            0
        }
    } else if (ipaddr.octets()[11] == 0xff) && (ipaddr.octets()[12] == 0xfe) { // EUI-64
        10 + flag_bonus(flags) + iface_bonus(iface)
    } else {
        8 + flag_bonus(flags) + iface_bonus(iface)
    }
}

fn compute_v4_metric(ipaddr: &Ipv4Addr, iface: &str, flags: &Vec<AddressHeaderFlag>, accept_private: bool) -> u8 {
    if ipaddr.is_loopback() || ipaddr.is_unspecified() { 0 }
    else if matches!(ipaddr.octets(), [192, 0, 2, _] | [198, 51, 100, _] | [203, 0, 113, _]) { 0 } // .is_documentation()
    else if ipaddr.is_link_local() { 0 }
    else if ipaddr.is_private() {
        if accept_private { 2 + flag_bonus(flags) + iface_bonus(iface) } else { 0 }
    } else { 8 + flag_bonus(flags) + iface_bonus(iface) }
}

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct IfaceIpAddr {
    addr: IpAddr,
    prefix_len: u8
}
impl std::fmt::Display for IfaceIpAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}/{})", self.addr, self.prefix_len)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct IfaceIpAddrData {
    iface: String,
    subnet_addr: IpAddr,
    metric: u8
}

fn get_subnet_addr(ip: &IpAddr, prefix_len:u8) -> IpAddr {
    match ip {
        IpAddr::V6(ip6) => {
            let netmask: u128 = (!0) << (128 - prefix_len);
            IpAddr::V6(ip6 & Ipv6Addr::from(netmask))
        },
        IpAddr::V4(ip4) => {
            let netmask: u32 = (!0) << (32 - prefix_len);
            IpAddr::V4(ip4 & Ipv4Addr::from(netmask))
        }
    }
}

async fn get_iface_addrs(handle: &Handle, conf: &Config) -> HashMap<IfaceIpAddr, IfaceIpAddrData>{
    let iface_map = match nl_get_iface_map(&handle).await {
        Ok(res) => res,
        Err(_) => {
            error!("Error while getting interfaces: nl_get_iface_map");
            HashMap::new()
        }
    };
    let addr_vec = match nl_get_addrs(&handle).await {
        Ok(res) => res,
        Err(_) => {
            error!("Error while getting IP addresses: nl_get_addrs");
            Vec::new()
        }
    };

    let mut ifaddrs = HashMap::new();
    for (ip, iphdr) in addr_vec.iter() {
        match iface_map.get(&iphdr.index) {
            Some(iface) => {
                if match_iface_pattern(iface, &conf.interfaces) {
                    let m = match ip {
                        IpAddr::V6(ip6) => compute_v6_metric(&ip6, iface, &iphdr.flags, conf.report_ipv6_ula),
                        IpAddr::V4(ip4) => compute_v4_metric(&ip4, iface, &iphdr.flags, conf.report_ipv4_private)
                    };
                    ifaddrs.insert(IfaceIpAddr {addr: ip.clone(), prefix_len: iphdr.prefix_len},
                        IfaceIpAddrData {iface: iface.to_string(), subnet_addr: get_subnet_addr(ip, iphdr.prefix_len), metric: m});
                } else {
                    info!("Interace not matched filter, ignoring {:?} hdr {:?} iface {}", ip, iphdr, iface);
                }
            },
            None => warn!("Unknown inface for {:?} hdr {:?}", ip, iphdr)
        }
    }
    ifaddrs
}

async fn recv_netlink_events(tx: Sender<HashMap<IfaceIpAddr, IfaceIpAddrData>>, conf: Config, oneshot: bool) -> () {
    let (mut conn, handle, mut messages) = new_connection().expect("Failed to make netlink connection. Check privileges.");

    let groups = RTNLGRP_LINK
        | RTNLGRP_IPV4_IFADDR
        | RTNLGRP_IPV6_IFADDR;

    let addr = SocketAddr::new(0, groups);
    conn.socket_mut()
        .socket_mut()
        .bind(&addr)
        .expect("Failed to bind to netlink socket.");

    tokio::spawn(conn);

    let mut iface_addrs_map = get_iface_addrs(&handle, &conf).await;
    info!("Trigger the first update");
    debug!("{:?}", iface_addrs_map);
    tx.send(iface_addrs_map.clone()).await.expect("Failed sending event over MPSC channel");

    if oneshot {
        return
    }

    while let Some((nlmsg, _)) = messages.next().await {
        debug!("netlink recv: {:?}", nlmsg.payload);
        match nlmsg.payload {
            InnerMessage(NewAddress(nladdrmsg)) => {
                for attr in nladdrmsg.attributes {
                    if let AddressAttribute::Address(addr) = attr {
                        let received_ifaceaddr = IfaceIpAddr {addr: addr.clone(), prefix_len: nladdrmsg.header.prefix_len};
                        if iface_addrs_map.contains_key(&received_ifaceaddr) {
                            info!("NewAddress notify for already known addr: {} -> ignore", received_ifaceaddr);
                        } else {
                            info!("NewAddress notify for uknown addr: {} -> trigger update", received_ifaceaddr);

                            iface_addrs_map = get_iface_addrs(&handle, &conf).await;
                            debug!("{:?}", iface_addrs_map);
                            tx.send(iface_addrs_map.clone()).await.expect("Failed sending event over MPSC channel");
                        }
                    }
                }
            },
            _ => {
                info!("Other type notify -> trigger update");
                iface_addrs_map = get_iface_addrs(&handle, &conf).await;
                debug!("{:?}", iface_addrs_map);
                tx.send(iface_addrs_map.clone()).await.expect("Failed sending event over MPSC channel");
            }
        }
    }
}

fn select_best(iface_addrs_map: &HashMap<IfaceIpAddr, IfaceIpAddrData>, conf: &Config) -> (Option<Ipv6Addr>, Option<Ipv4Addr>) {
    let mut best6 = None;
    let mut best4 = None;
    let mut metric6 = 0;
    let mut metric4 = 0;
    for (ip, ipdata) in iface_addrs_map.iter() {
        match ip.addr {
            IpAddr::V6(ip6) => {
                if conf.enable_ipv6 && ipdata.metric > metric6 {
                    best6 = Some(ip6);
                    metric6 = ipdata.metric;
                }
            },
            IpAddr::V4(ip4) => {
                if conf.enable_ipv4 && ipdata.metric > metric4 {
                    best4 = Some(ip4);
                    metric4 = ipdata.metric;
                }
            },
        }
    }
    (best6, best4)
}

fn write_nft_sets(filename: &str, state: &HashMap<IfaceIpAddr, IfaceIpAddrData>) {
    info!("Writing NFT sets to file {} .", filename);
    let file = match File::create(filename) { Ok(f) => f, Err(e) => { error!("{:?}", e); return; }};
    let mut wrt = LineWriter::new(file);
    let mut dedup4: HashMap<Ipv4Addr, u8> = HashMap::new();
    let mut dedup6: HashMap<Ipv6Addr, u8> = HashMap::new();

    match wrt.write_all(b"define LOCAL_NET4={\n") { Ok(()) => (), Err(e) => { error!("{:?}", e); return; }};
    let mut is_first = true;
    for (ip,ipdata) in state.into_iter() {
        if let IpAddr::V4(ip4) = ipdata.subnet_addr {
            match dedup4.get(&ip4) {
                Some(dedup_pfxlen) => {
                    if *dedup_pfxlen == ip.prefix_len {
                        debug!("Deduped {:?}/{}", &ip4, dedup_pfxlen);
                        continue;
                    } else {
                        dedup4.insert(ip4, ip.prefix_len);
                    }
                },
                None => {
                    dedup4.insert(ip4, ip.prefix_len);
                }
            }

            if is_first {
                is_first = false;
            } else {
                match wrt.write_all(b",\n") { Ok(()) => (), Err(e) => { error!("{:?}", e); return; }};
            }
            match write!(wrt, "{}/{}", ip4.to_string(), ip.prefix_len) {
                Ok(()) => (), Err(e) => { error!("{:?}", e); return; }};
        }
    }
    match wrt.write_all(b"\n}\n") { Ok(()) => (), Err(e) => { error!("{:?}", e); return; }};

    match wrt.write_all(b"define LOCAL_NET6={\n") { Ok(()) => (), Err(e) => { error!("{:?}", e); return; }};
    is_first = true;
    for (ip,ipdata) in state.into_iter() {
        if let IpAddr::V6(ip6) = ipdata.subnet_addr {
            match dedup6.get(&ip6) {
                Some(dedup_pfxlen) => {
                    if *dedup_pfxlen == ip.prefix_len {
                        debug!("Deduped {:?}/{}", &ip6, dedup_pfxlen);
                        continue;
                    } else {
                        dedup6.insert(ip6, ip.prefix_len);
                    }
                },
                None => {
                    dedup6.insert(ip6, ip.prefix_len);
                }
            }

            if is_first {
                is_first = false;
            } else {
                match wrt.write_all(b",\n") { Ok(()) => (), Err(e) => { error!("{:?}", e); return; }};
            }
            match write!(wrt, "{}/{}", ip6.to_string(), ip.prefix_len) {
                Ok(()) => (), Err(e) => { error!("{:?}", e); return; }};
        }
    }
    match wrt.write_all(b"\n}\n") { Ok(()) => (), Err(e) => { error!("{:?}", e); return; }};
}

async fn shell(cmd: &str) {
    info!("Running shell command: {}", cmd);
    match Command::new("sh").arg("-c").arg(cmd).output().await {
        Ok(output) => {
            info!("shell {} executed successfully: {:?}", cmd, output);
        },
        Err(err) => {
            warn!("shell {} failed: {}", cmd, err);
        }
    }
}

async fn write_nft_and_notify(new_state: &HashMap<IfaceIpAddr, IfaceIpAddrData>, conf: &Config) {
    if let Some(hooks) = conf.hooks.as_ref() {
        for hook in hooks.iter() {
            info!("Woking on hook {:?}", hook);
            if let Some(outfile) = hook.nft_sets_outfile.as_ref() {
                write_nft_sets(outfile, new_state);
            }
            if let Some(cmd) = hook.shell.as_ref() {
                shell(cmd).await;
            }
        }
    }
}

async fn send_ddns_update(ip: IpAddr, conf: &Config) -> Result<(), String> {
    info!("Sending DDNS: {:?}", ip);

    let params = [
        ("myip", match ip {
                            IpAddr::V6(ip6) => ip6.to_string(),
                            IpAddr::V4(ip4) => ip4.to_string()
                        }),
        ("hostname", conf.hostname.to_string())
    ];
    let url = reqwest::Url::parse_with_params(&conf.ddns_url, &params)
        .expect(format!("Can not parse URL {} with params {:?}", &conf.ddns_url, &params).as_str());
    match reqwest::get(url.clone()).await {
        Ok(res) => {
            let status = res.status();
            match status {
                http::StatusCode::OK => {
                    let rtext = res.text().await;
                    info!("DDNS GET to URL {} succeeded with code: {}, text: {:?}",
                        url, status, &rtext);
                    Ok(())
                },
                _ => {
                    let rtext = res.text().await;
                    warn!("DDNS GET to URL {} failed with code: {}, text: {:?}",
                        url, status, &rtext);
                    Err(format!("Code: {}, text: {:?}", status, &rtext))
                }
            }
        },
        Err(err) => {
            warn!("DDNS GET to URL {} failed: {}", url, err);
            Err(err.to_string())
        }
    }
}

async fn worker(mut rx: Receiver<HashMap<IfaceIpAddr, IfaceIpAddrData>>, conf: Config, oneshot: bool) {
    let mut start = SystemTime::now();
    let mut current_state: Option<HashMap<IfaceIpAddr, IfaceIpAddrData>> = None;
    let mut current_best4 = None;
    let mut current_best6 = None;

    while let Some(iface_addrs_map) = rx.recv().await {
        let elapsed = start.elapsed().expect("Time computation error.").as_secs();
        let mut new_state = Some(iface_addrs_map);

        if elapsed < MIN_UPDATE_INTERVAL_S {
            debug!("dampening changes -> need to sleep for {}", MIN_UPDATE_INTERVAL_S - elapsed);
            sleep(Duration::from_secs(MIN_UPDATE_INTERVAL_S - elapsed)).await;
            while let Ok(iface_addrs_map) = rx.try_recv() {
                debug!("got more {:?}", iface_addrs_map);
                new_state = Some(iface_addrs_map);
            }
        }

        if new_state != current_state || current_state == None {
            debug!("Detected state change -> Running update");

            if let Some(new_state_data_ref) = new_state.as_ref() {
                write_nft_and_notify(new_state_data_ref, &conf).await;

                let (new_best6, new_best4) = select_best(new_state_data_ref, &conf);
                if new_best6 != current_best6 {
                    if let Some(new_best6_data) = new_best6 {
                        let _ = send_ddns_update(IpAddr::V6(new_best6_data), &conf).await;
                        current_best6 = new_best6;
                    }
                }
                if new_best4 != current_best4 {
                    if let Some(new_best4_data) = new_best4 {
                        let _ = send_ddns_update(IpAddr::V4(new_best4_data), &conf).await;
                        current_best4 = new_best4;
                    }
                }
            }

            current_state = new_state;
        }

        start = SystemTime::now();

        if oneshot {
            return
        }
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize, Clone)]
struct Hook {
    shell: Option<String>,
    nft_sets_outfile: Option<String>,
}

fn default_report_ipv4_private() -> bool {
    false
}

fn default_report_ipv6_ula() -> bool {
    false
}

#[derive(Debug, PartialEq, Serialize, Deserialize, Clone)]
struct Config {
    debug: Option<bool>,
    ddns_url: String,
    hostname: String,
    enable_ipv6: bool,
    enable_ipv4: bool,
    #[serde(default = "default_report_ipv4_private")]
    report_ipv4_private: bool,
    #[serde(default = "default_report_ipv6_ula")]
    report_ipv6_ula: bool,
    interfaces: Vec<String>,
    hooks: Option<Vec<Hook>>,
}

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Config file path
    #[arg(short, long, default_value_t=String::from("/etc/teleddns/teleddns.yaml"))]
    config: String,

    /// Do not deaemonize, run just one DDNS update & hooks.
    #[arg(short, long, default_value_t=false)]
    oneshot: bool,

    #[command(flatten)]
    verbose: clap_verbosity_flag::Verbosity<clap_verbosity_flag::InfoLevel>,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let cf = File::open(&args.config).expect("Unable to open configuration file.");
    let conf: Config = serde_yaml::from_reader(cf).expect("Failed to parse configuration file.");

    let loglevel = match conf.debug {
        Some(false) => { args.verbose.log_level_filter() },
        Some(true) => { log::LevelFilter::Debug },
        None => { args.verbose.log_level_filter() }
    };
    env_logger::builder()
        .filter_level(loglevel)
        .init();
    info!("Read config file {} finished", &args.config);
    info!("Set log level {:?}", loglevel);

    let mut jset = JoinSet::new();
    let (tx, rx) = mpsc::channel(10);

    jset.spawn(worker(rx, conf.clone(), args.oneshot));
    jset.spawn(recv_netlink_events(tx, conf.clone(), args.oneshot));

    if args.oneshot {
        info!("Waiting for the operation to finish");
    } else {
        info!("Listener started, waiting for HUP");
        signal(SignalKind::hangup()).expect("Failed to bind to HUP signal.").recv().await;
        info!("Terminating on HUP.");
        jset.shutdown().await;
    }

    let mut output = Vec::new();
    while let Some(res) = jset.join_next().await{
        match res {
            Ok(t) => output.push(t),
            Err(err) if err.is_panic() => panic::resume_unwind(err.into_panic()),
            Err(err) => panic!("{err}"),
        }
    }
    info!("Sucessfully shutdown");
    std::process::exit(0);
}
