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

use async_process::Command;
use clap::Parser;
use futures::stream::{StreamExt, TryStreamExt};
use log::{debug, error, info, warn};
use netlink_packet_core::NetlinkPayload::InnerMessage;
use netlink_packet_route::address::AddressHeader;
use netlink_packet_route::address::*;
use netlink_packet_route::link::*;
use netlink_packet_route::RouteNetlinkMessage::*;
use netlink_sys::{AsyncSocket, SocketAddr};
use rtnetlink::{new_connection, Error, Handle};
use serde_derive::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::prelude::*;
use std::io::LineWriter;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::panic;
use std::time::SystemTime;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::task::JoinSet;
use tokio::time::{timeout, Duration};

const MIN_UPDATE_INTERVAL_S: u64 = 30;
const HTTP_TIMEOUT_S: u64 = 30;

/// First retry delay after a transient DDNS failure; doubles up to `RETRY_MAX_S`.
/// (TODO.md §2 turns these into config keys.)
const RETRY_MIN_S: u64 = 60;
const RETRY_MAX_S: u64 = 3600;
/// `abuse`/HTTP 429 means a rate limit tripped. teleddns-server counts 60 updates
/// per hour per record set, so the first retry has to be an hour-scale wait --
/// a minute-scale retry would just trip the limit again.
const RETRY_RATELIMIT_MIN_S: u64 = 900;

const RTNLGRP_LINK: u32 = 1;
const RTNLGRP_IPV4_IFADDR: u32 = 1 << 4;
const RTNLGRP_IPV6_IFADDR: u32 = 1 << 8;
//const RTNLGRP_IPV4_NETCONF: u32 = 1 << 23;
//const RTNLGRP_IPV6_NETCONF: u32 = 1 << 24;

async fn nl_get_addrs(handle: &Handle) -> Result<Vec<(IpAddr, AddressHeader)>, Error> {
    let mut aget = handle.address().get().execute();
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
        if msg.header.flags.contains(LinkFlags::Up)
            && msg.header.flags.contains(LinkFlags::LowerUp)
            && msg.header.flags.contains(LinkFlags::Running)
        {
            is_up = true;
        }
        for nla in msg.attributes.into_iter() {
            match nla {
                LinkAttribute::IfName(ifn) => {
                    ifname = Some(ifn);
                }
                LinkAttribute::OperState(State::Up) => {
                    is_up = true;
                }
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
        if ptr == &negative {
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

// acceptable values 0,8,16,32
fn iface_bonus(iface: &str) -> u8 {
    if iface.starts_with("en") {
        16
    } else if iface.starts_with("wl") {
        8
    } else {
        0
    }
}

// acceptable values 0..7
fn flag_bonus(flags: &AddressHeaderFlags) -> u8 {
    // Warn: i.e. OpenVPN tunnels have permanent bonus while the real interfaces
    // may have just SLAAC address
    let mut b = 0;
    if flags.contains(AddressHeaderFlags::Permanent) {
        b += 1;
    }
    b
}

fn compute_v6_metric(
    ipaddr: &Ipv6Addr,
    iface: &str,
    flags: &AddressHeaderFlags,
    accept_ula: bool,
) -> u8 {
    if ipaddr.is_unicast_link_local() || ipaddr.is_multicast() {
        0
    } else if matches!(ipaddr.segments(), [0, 0, 0, 0, 0, 0xffff, _, _]) {
        // .is_ipv4_mapped
        0
    } else if ipaddr.is_loopback() || ipaddr.is_unspecified() {
        0
    } else if matches!(
        ipaddr.segments(),
        [0x2001, 0xdb8, ..] | [0x3fff, 0..=0x0fff, ..]
    ) {
        // .is_documentation
        0
    } else if (ipaddr.segments()[0] == 0x2001)
        && (ipaddr.segments()[1] == 0x2)
        && (ipaddr.segments()[2] == 0)
    {
        // .is_benchmarking
        0
    } else if ipaddr.is_unique_local() {
        if accept_ula {
            1 + iface_bonus(iface) + flag_bonus(flags)
        } else {
            0
        }
    } else if flags.contains(AddressHeaderFlags::Deprecated) {
        1 + iface_bonus(iface) + flag_bonus(flags)
    } else if (ipaddr.octets()[11] == 0xff) && (ipaddr.octets()[12] == 0xfe) {
        // EUI-64
        129 + iface_bonus(iface) + flag_bonus(flags)
    } else {
        128 + iface_bonus(iface) + flag_bonus(flags)
    }
}

fn compute_v4_metric(
    ipaddr: &Ipv4Addr,
    iface: &str,
    flags: &AddressHeaderFlags,
    accept_private: bool,
) -> u8 {
    if ipaddr.is_loopback() || ipaddr.is_unspecified() {
        0
    } else if matches!(
        ipaddr.octets(),
        [192, 0, 2, _] | [198, 51, 100, _] | [203, 0, 113, _]
    ) {
        // [IANA TEST-NET-1, IANA documentation, IANA TEST-NET-3]
        // .is_documentation()
        0
    } else if ipaddr.is_link_local() {
        0
    } else if ipaddr.is_private() {
        if accept_private {
            1 + iface_bonus(iface) + flag_bonus(flags)
        } else {
            0
        }
    } else {
        128 + iface_bonus(iface) + flag_bonus(flags)
    }
}

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct IfaceIpAddr {
    addr: IpAddr,
    prefix_len: u8,
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
    metric: u8,
    flags: AddressHeaderFlags,
}

fn get_subnet_addr(ip: &IpAddr, prefix_len: u8) -> IpAddr {
    match ip {
        IpAddr::V6(ip6) => {
            let netmask: u128 = (!0) << (128 - prefix_len);
            IpAddr::V6(ip6 & Ipv6Addr::from(netmask))
        }
        IpAddr::V4(ip4) => {
            let netmask: u32 = (!0) << (32 - prefix_len);
            IpAddr::V4(ip4 & Ipv4Addr::from(netmask))
        }
    }
}

async fn get_iface_addrs(handle: &Handle, conf: &Config) -> HashMap<IfaceIpAddr, IfaceIpAddrData> {
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
                        IpAddr::V6(ip6) => {
                            compute_v6_metric(&ip6, iface, &iphdr.flags, conf.report_ipv6_ula)
                        }
                        IpAddr::V4(ip4) => {
                            compute_v4_metric(&ip4, iface, &iphdr.flags, conf.report_ipv4_private)
                        }
                    };
                    ifaddrs.insert(
                        IfaceIpAddr {
                            addr: ip.clone(),
                            prefix_len: iphdr.prefix_len,
                        },
                        IfaceIpAddrData {
                            iface: iface.to_string(),
                            subnet_addr: get_subnet_addr(ip, iphdr.prefix_len),
                            metric: m,
                            flags: iphdr.flags,
                        },
                    );
                } else {
                    info!(
                        "Interace not matched filter, ignoring {:?} hdr {:?} iface {}",
                        ip, iphdr, iface
                    );
                }
            }
            None => warn!("Unknown inface for {:?} hdr {:?}", ip, iphdr),
        }
    }
    ifaddrs
}

async fn netlink_events_receiver(
    tx: Sender<HashMap<IfaceIpAddr, IfaceIpAddrData>>,
    conf: Config,
) -> () {
    let (mut conn, handle, mut messages) =
        new_connection().expect("Failed to make netlink connection. Check privileges.");

    let groups = RTNLGRP_LINK | RTNLGRP_IPV4_IFADDR | RTNLGRP_IPV6_IFADDR;

    let addr = SocketAddr::new(0, groups);
    conn.socket_mut()
        .socket_mut()
        .bind(&addr)
        .expect("Failed to bind to netlink socket.");

    tokio::spawn(conn);

    let mut iface_addrs_map = get_iface_addrs(&handle, &conf).await;
    info!("Trigger the first update");
    debug!("{:?}", iface_addrs_map);
    tx.send(iface_addrs_map.clone())
        .await
        .expect("Failed sending event over MPSC channel");

    while let Some((nlmsg, _)) = messages.next().await {
        debug!("netlink recv: {:?}", nlmsg.payload);
        match nlmsg.payload {
            InnerMessage(NewAddress(nladdrmsg)) => {
                for attr in nladdrmsg.attributes {
                    if let AddressAttribute::Address(addr) = attr {
                        let received_ifaceaddr = IfaceIpAddr {
                            addr: addr.clone(),
                            prefix_len: nladdrmsg.header.prefix_len,
                        };
                        let addr_changed = match iface_addrs_map.get(&received_ifaceaddr) {
                            Some(existing) => {
                                if existing.flags != nladdrmsg.header.flags {
                                    debug!(
                                        "NewAddress notify for known addr with changed flags: {} {:?} -> {:?} -> trigger update",
                                        received_ifaceaddr, existing.flags, nladdrmsg.header.flags
                                    );
                                    true
                                } else {
                                    debug!(
                                        "NewAddress notify for already known addr: {} -> ignore",
                                        received_ifaceaddr
                                    );
                                    false
                                }
                            }
                            None => {
                                debug!(
                                    "NewAddress notify for unknown addr: {} -> trigger update",
                                    received_ifaceaddr
                                );
                                true
                            }
                        };
                        if addr_changed {
                            iface_addrs_map = get_iface_addrs(&handle, &conf).await;
                            debug!("{:?}", iface_addrs_map);
                            tx.send(iface_addrs_map.clone())
                                .await
                                .expect("Failed sending event over MPSC channel");
                        }
                    }
                }
            }
            _ => {
                debug!("Other type notify -> trigger update");
                iface_addrs_map = get_iface_addrs(&handle, &conf).await;
                debug!("{:?}", iface_addrs_map);
                tx.send(iface_addrs_map.clone())
                    .await
                    .expect("Failed sending event over MPSC channel");
            }
        }
    }
}

fn select_best(
    iface_addrs_map: &HashMap<IfaceIpAddr, IfaceIpAddrData>,
    conf: &Config,
) -> (Option<Ipv6Addr>, Option<Ipv4Addr>) {
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
            }
            IpAddr::V4(ip4) => {
                if conf.enable_ipv4 && ipdata.metric > metric4 {
                    best4 = Some(ip4);
                    metric4 = ipdata.metric;
                }
            }
        }
    }
    (best6, best4)
}

fn write_nft_sets(filename: &str, state: &HashMap<IfaceIpAddr, IfaceIpAddrData>) {
    info!("Writing NFT sets to file {} .", filename);
    let file = match File::create(filename) {
        Ok(f) => f,
        Err(e) => {
            error!("Failed to write NFT sets: {:?}", e);
            return;
        }
    };
    let mut wrt = LineWriter::new(file);
    let mut dedup4: HashMap<Ipv4Addr, u8> = HashMap::new();
    let mut dedup6: HashMap<Ipv6Addr, u8> = HashMap::new();

    match wrt.write_all(b"define LOCAL_NET4={\n") {
        Ok(()) => (),
        Err(e) => {
            error!("{:?}", e);
            return;
        }
    };
    let mut is_first = true;
    for (ip, ipdata) in state.into_iter() {
        if let IpAddr::V4(ip4) = ipdata.subnet_addr {
            match dedup4.get(&ip4) {
                Some(dedup_pfxlen) => {
                    if *dedup_pfxlen == ip.prefix_len {
                        debug!("Deduped {:?}/{}", &ip4, dedup_pfxlen);
                        continue;
                    } else {
                        dedup4.insert(ip4, ip.prefix_len);
                    }
                }
                None => {
                    dedup4.insert(ip4, ip.prefix_len);
                }
            }

            if is_first {
                is_first = false;
            } else {
                match wrt.write_all(b",\n") {
                    Ok(()) => (),
                    Err(e) => {
                        error!("{:?}", e);
                        return;
                    }
                };
            }
            match write!(wrt, "{}/{}", ip4.to_string(), ip.prefix_len) {
                Ok(()) => (),
                Err(e) => {
                    error!("{:?}", e);
                    return;
                }
            };
        }
    }
    match wrt.write_all(b"\n}\n") {
        Ok(()) => (),
        Err(e) => {
            error!("{:?}", e);
            return;
        }
    };

    match wrt.write_all(b"define LOCAL_NET6={\n") {
        Ok(()) => (),
        Err(e) => {
            error!("{:?}", e);
            return;
        }
    };
    is_first = true;
    for (ip, ipdata) in state.into_iter() {
        if let IpAddr::V6(ip6) = ipdata.subnet_addr {
            match dedup6.get(&ip6) {
                Some(dedup_pfxlen) => {
                    if *dedup_pfxlen == ip.prefix_len {
                        debug!("Deduped {:?}/{}", &ip6, dedup_pfxlen);
                        continue;
                    } else {
                        dedup6.insert(ip6, ip.prefix_len);
                    }
                }
                None => {
                    dedup6.insert(ip6, ip.prefix_len);
                }
            }

            if is_first {
                is_first = false;
            } else {
                match wrt.write_all(b",\n") {
                    Ok(()) => (),
                    Err(e) => {
                        error!("{:?}", e);
                        return;
                    }
                };
            }
            match write!(wrt, "{}/{}", ip6.to_string(), ip.prefix_len) {
                Ok(()) => (),
                Err(e) => {
                    error!("{:?}", e);
                    return;
                }
            };
        }
    }
    match wrt.write_all(b"\n}\n") {
        Ok(()) => (),
        Err(e) => {
            error!("{:?}", e);
            return;
        }
    };
}

async fn shell(cmd: &str) {
    info!("Running shell command: {}", cmd);
    match Command::new("sh").arg("-c").arg(cmd).output().await {
        Ok(output) => {
            info!("shell {} executed successfully: {:?}", cmd, output);
        }
        Err(err) => {
            warn!("shell {} failed: {}", cmd, err);
        }
    }
}

async fn apply_hooks(new_state: &HashMap<IfaceIpAddr, IfaceIpAddrData>, conf: &Config) {
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

fn sanitize_url(url: &reqwest::Url) -> String {
    let u = url.to_string();
    let ps = u.match_indices(':').nth(1);
    let pe = u.find('@');

    if let Some((psi, _)) = ps {
        if let Some(pei) = pe {
            return format!("{}:<PASSWORD>{}", &u[0..psi], &u[pei..]);
        }
    }
    u
}

/// Classified result of a dyndns2 update request. See DYNDNS2.md for the full
/// protocol notes and the keyword -> class mapping.
#[derive(Debug)]
enum DdnsOutcome {
    /// Update accepted and applied (`good`), with the addresses the server
    /// reports as now published for the name (may include the other family).
    Good(Vec<IpAddr>),
    /// Address already current (`nochg`) — treated as success.
    NoChg(Vec<IpAddr>),
    /// Permanent failure — retrying will not help (`badauth`, `nohost`,
    /// `!yours`, `notfqdn`, `numhost`, ...). Do not spin on it.
    Permanent(String),
    /// Transient failure — worth retrying (network/transport error, 5xx,
    /// `911`/`dnserr`).
    Transient(String),
    /// Rate limit tripped (`abuse` / HTTP 429) — retryable, but only after a
    /// long wait, see `RETRY_RATELIMIT_MIN_S`.
    RateLimited(String),
}

impl DdnsOutcome {
    fn is_success(&self) -> bool {
        matches!(self, DdnsOutcome::Good(_) | DdnsOutcome::NoChg(_))
    }

    /// Shortest acceptable delay before this failure may be retried. Permanent
    /// failures are never retried, so they have no floor.
    fn retry_floor_s(&self) -> u64 {
        match self {
            DdnsOutcome::RateLimited(_) => RETRY_RATELIMIT_MIN_S,
            _ => RETRY_MIN_S,
        }
    }
}

/// Parse the informational remainder of a `good`/`nochg` line: the addresses now
/// on record, IPv4 first, comma-separated. Unparseable tokens are skipped — the
/// field is advisory, a server is free to word it differently.
fn parse_ddns_addresses(rest: &str) -> Vec<IpAddr> {
    rest.split([',', ' ', '\t'])
        .filter_map(|t| t.trim().parse::<IpAddr>().ok())
        .collect()
}

/// Validate the configured hostname against the name syntax the server accepts
/// (DYNDNS2.md §4): dot-separated labels of 1–63 characters from `[A-Za-z0-9-_]`,
/// optionally a leading `*` wildcard label, a trailing dot tolerated. Anything
/// else would come back as `notfqdn`, so we catch it at startup instead.
///
/// A comma-separated list is rejected on purpose: the server would accept up to 20
/// names, but this client publishes one name and reads one result line, so a list
/// is a configuration mistake rather than a supported feature.
fn validate_hostname(hostname: &str) -> Result<(), String> {
    if hostname.is_empty() {
        return Err("hostname is empty".to_string());
    }
    if hostname.contains(',') {
        return Err(
            "hostname must be a single FQDN; comma-separated lists are not supported".to_string(),
        );
    }
    if hostname.chars().any(char::is_whitespace) {
        return Err("hostname must not contain whitespace".to_string());
    }
    let name = hostname.strip_suffix('.').unwrap_or(hostname);
    if !name.contains('.') {
        return Err(format!("{name:?} is not a fully qualified domain name"));
    }
    for (i, label) in name.split('.').enumerate() {
        if label.is_empty() || label.len() > 63 {
            return Err(format!("label {label:?} must be 1–63 characters long"));
        }
        if i == 0 && label == "*" {
            continue; // a leading wildcard label is allowed
        }
        if let Some(c) = label
            .chars()
            .find(|c| !c.is_ascii_alphanumeric() && *c != '-' && *c != '_')
        {
            return Err(format!(
                "label {label:?} contains an invalid character {c:?}"
            ));
        }
    }
    Ok(())
}

/// Build the shared HTTP client. A stable `User-Agent` is important because some
/// dyndns2 servers reject a missing/generic UA with `badagent`; the request
/// timeout keeps a hung server from blocking the update loop.
fn build_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("teleddns/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(HTTP_TIMEOUT_S))
        .build()
        .expect("Failed to build HTTP client")
}

/// Classify a dyndns2 response (DYNDNS2.md §8, §12.2). The body carries one line
/// per requested hostname, and a request-level failure is a single line; since we
/// always send exactly one hostname, our result is the first non-empty line. Its
/// first whitespace-delimited token is the keyword (matched case-insensitively),
/// the remainder is informational. The HTTP status is only a fallback, for servers
/// that answer with an unrecognized or empty body.
fn classify_ddns_response(status: reqwest::StatusCode, body: &str) -> DdnsOutcome {
    let line = body
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let (keyword, rest) = match line.split_once(char::is_whitespace) {
        Some((kw, rest)) => (kw.to_ascii_lowercase(), rest),
        None => (line.to_ascii_lowercase(), ""),
    };
    match keyword.as_str() {
        "good" => DdnsOutcome::Good(parse_ddns_addresses(rest)),
        "nochg" => DdnsOutcome::NoChg(parse_ddns_addresses(rest)),
        // Rate limit tripped — retry, but only after a long back-off (§10).
        "abuse" => DdnsOutcome::RateLimited(line.to_string()),
        // Permanent dyndns2 failures — retrying will not help. `numhost`,
        // `badagent` and `notfqdn` mean the request itself is wrong; `ko` is
        // DuckDNS' generic rejection (sent with HTTP 200).
        "nohost" | "notfqdn" | "nofqdn" | "numhost" | "!yours" | "badauth" | "badagent"
        | "!donator" | "!active" | "ko" => DdnsOutcome::Permanent(line.to_string()),
        // Transient dyndns2 failures — worth retrying.
        "911" | "dnserr" => DdnsOutcome::Transient(line.to_string()),
        // Unknown/empty body: decide on the HTTP status instead. This is also the
        // path that keeps pre-0.4.0 teleddns-server (JSON body, HTTP 200) working.
        _ => {
            if status.is_success() {
                DdnsOutcome::Good(Vec::new())
            } else if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                DdnsOutcome::RateLimited(format!("HTTP {} {}", status, line))
            } else if status.is_server_error() {
                DdnsOutcome::Transient(format!("HTTP {} {}", status, line))
            } else {
                DdnsOutcome::Permanent(format!("HTTP {} {}", status, line))
            }
        }
    }
}

/// Build the update URL for one address. dyndns2: IPv4 goes in `myip`, IPv6 in
/// `myipv6`. Exactly one hostname and exactly one address per request: the server
/// also accepts comma-separated lists, but one family per request is what gives us
/// a per-family result (DYNDNS2.md §9) instead of a merged line we cannot attribute.
fn build_update_url(base: &str, ip: IpAddr, hostname: &str) -> Result<reqwest::Url, String> {
    let (ip_param, ip_value) = match ip {
        IpAddr::V4(ip4) => ("myip", ip4.to_string()),
        IpAddr::V6(ip6) => ("myipv6", ip6.to_string()),
    };
    let params = [(ip_param, ip_value), ("hostname", hostname.to_string())];
    reqwest::Url::parse_with_params(base, &params)
        .map_err(|e| format!("can not parse URL {base} with params {params:?}: {e}"))
}

async fn send_ddns_update(client: &reqwest::Client, ip: IpAddr, conf: &Config) -> DdnsOutcome {
    info!("Sending DDNS: {:?}", ip);

    let url = match build_update_url(&conf.ddns_url, ip, &conf.hostname) {
        Ok(u) => u,
        Err(e) => {
            error!("{}", e);
            return DdnsOutcome::Permanent(format!("invalid URL: {e}"));
        }
    };
    let sanitized_url = sanitize_url(&url);

    match client.get(url).send().await {
        Ok(res) => {
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            let outcome = classify_ddns_response(status, &body);
            match &outcome {
                DdnsOutcome::Good(addrs) | DdnsOutcome::NoChg(addrs) => {
                    info!(
                        "DDNS GET to URL {} succeeded: HTTP {}, body: {:?}",
                        sanitized_url,
                        status,
                        body.trim()
                    );
                    // `good`/`nochg` report what is now on record, so we can check
                    // that the server really published what we asked for.
                    if !addrs.is_empty() && !addrs.contains(&ip) {
                        warn!(
                            "DDNS server reported success but published {:?} for {}, \
                             which does not include the address we sent ({})",
                            addrs, conf.hostname, ip
                        );
                    }
                }
                DdnsOutcome::Permanent(m) => error!(
                    "DDNS GET to URL {} permanent failure: HTTP {}, {} \
                     (not retrying until the address or the configuration changes)",
                    sanitized_url, status, m
                ),
                DdnsOutcome::RateLimited(m) => warn!(
                    "DDNS GET to URL {} rate limited: HTTP {}, {}",
                    sanitized_url, status, m
                ),
                DdnsOutcome::Transient(m) => warn!(
                    "DDNS GET to URL {} transient failure: HTTP {}, {}",
                    sanitized_url, status, m
                ),
            }
            outcome
        }
        Err(err) => {
            warn!(
                "DDNS GET to URL {} failed (transport): {}",
                sanitized_url, err
            );
            DdnsOutcome::Transient(err.to_string())
        }
    }
}

/// Per-family push state. Keeps what the server acknowledged and paces retries as
/// DYNDNS2.md §12.3 requires: `good`/`nochg` are adopted and not re-sent, transient
/// failures back off exponentially, and a permanent failure is not retried at all
/// until the desired address (or the configuration) changes.
#[derive(Debug)]
struct PushState<A> {
    /// Last value the server acknowledged.
    confirmed: Option<A>,
    /// Value the server rejected permanently — do not send it again.
    rejected: Option<A>,
    /// Earliest time a retry may be attempted after a transient failure.
    next_retry: Option<SystemTime>,
    /// Current back-off; 0 until the first transient failure.
    backoff_s: u64,
}

// Hand-written so that `A` need not implement `Default` (`Ipv4Addr` does not).
impl<A> Default for PushState<A> {
    fn default() -> Self {
        PushState {
            confirmed: None,
            rejected: None,
            next_retry: None,
            backoff_s: 0,
        }
    }
}

impl<A: Copy + PartialEq + Into<IpAddr> + std::fmt::Display> PushState<A> {
    /// True when the server state already matches what we want to publish.
    fn is_current(&self, desired: Option<A>) -> bool {
        self.confirmed == desired
    }

    /// Push `desired` if it differs from what the server acknowledged and the
    /// retry policy allows an attempt right now.
    async fn sync(&mut self, desired: Option<A>, client: &reqwest::Client, conf: &Config) {
        let Some(addr) = desired else {
            // Losing an address is not an update: the DDNS endpoint never deletes
            // a record set (§6). Just forget what we published.
            self.confirmed = None;
            self.clear_retry();
            return;
        };
        if self.confirmed == Some(addr) {
            return;
        }
        if self.rejected == Some(addr) {
            debug!(
                "Not re-sending {} — the server rejected it permanently",
                addr
            );
            return;
        }
        if let Some(next) = self.next_retry {
            if SystemTime::now() < next {
                debug!(
                    "Holding back the update of {} — retry back-off pending",
                    addr
                );
                return;
            }
        }

        let outcome = send_ddns_update(client, addr.into(), conf).await;
        if outcome.is_success() {
            self.confirmed = Some(addr);
            self.rejected = None;
            self.clear_retry();
        } else if let DdnsOutcome::Permanent(_) = outcome {
            self.rejected = Some(addr);
            self.clear_retry();
        } else {
            let delay = self.backoff_s.max(outcome.retry_floor_s()).min(RETRY_MAX_S);
            self.next_retry = Some(SystemTime::now() + Duration::from_secs(delay));
            self.backoff_s = (delay * 2).min(RETRY_MAX_S);
            warn!("Retrying the update of {} in {} s", addr, delay);
        }
    }

    fn clear_retry(&mut self) {
        self.next_retry = None;
        self.backoff_s = 0;
    }
}

async fn update_worker(mut rx: Receiver<HashMap<IfaceIpAddr, IfaceIpAddrData>>, conf: Config) {
    let client = build_http_client();
    let mut iteration_start = SystemTime::now();
    let mut current_iface_addrs_map = None;
    let mut state4: PushState<Ipv4Addr> = PushState::default();
    let mut state6: PushState<Ipv6Addr> = PushState::default();
    let (mut pending_best4, mut pending_best6) = (None, None);
    let mut new_iface_addrs_map = None;
    let mut main_loop_next_timeout = MIN_UPDATE_INTERVAL_S + 1;

    loop {
        match timeout(Duration::from_secs(main_loop_next_timeout), rx.recv()).await {
            Ok(Some(iface_addrs_map)) => {
                new_iface_addrs_map = Some(iface_addrs_map);
                let elapsed = iteration_start
                    .elapsed()
                    .unwrap_or(Duration::ZERO)
                    .as_secs();
                if elapsed < MIN_UPDATE_INTERVAL_S {
                    main_loop_next_timeout = MIN_UPDATE_INTERVAL_S - elapsed + 1;
                    debug!(
                        "dampening changes, elapsed: {}, next loop timeout: {}",
                        elapsed, main_loop_next_timeout
                    );
                    continue;
                }
            }
            Ok(None) => {
                info!("Channel closed, stopping update worker!");
                break;
            }
            Err(_) => {
                // timeout, continue with existing state
            }
        }

        if new_iface_addrs_map != current_iface_addrs_map {
            info!("Detected state change -> Running update");

            if let Some(new_iface_addrs_map_data) = new_iface_addrs_map.as_ref() {
                apply_hooks(new_iface_addrs_map_data, &conf).await;
                (pending_best6, pending_best4) = select_best(new_iface_addrs_map_data, &conf);
            }
            current_iface_addrs_map = new_iface_addrs_map.clone();
        } else if !state6.is_current(pending_best6) || !state4.is_current(pending_best4) {
            debug!("Pending DDNS updates -> re-evaluating (subject to the retry policy)");
        }

        state6.sync(pending_best6, &client, &conf).await;
        state4.sync(pending_best4, &client, &conf).await;

        iteration_start = SystemTime::now();
        main_loop_next_timeout = MIN_UPDATE_INTERVAL_S + 1;
    }
}

/// One update round for `--oneshot`. Returns false when a DDNS push was attempted
/// and did not succeed, so the caller can exit non-zero.
async fn oneshot(conf: &Config) -> bool {
    let (mut conn, handle, _messages) =
        new_connection().expect("Failed to make netlink connection. Check privileges.");

    let groups = RTNLGRP_LINK | RTNLGRP_IPV4_IFADDR | RTNLGRP_IPV6_IFADDR;

    let addr = SocketAddr::new(0, groups);
    conn.socket_mut()
        .socket_mut()
        .bind(&addr)
        .expect("Failed to bind to netlink socket.");

    tokio::spawn(conn);

    let iface_addrs_map = get_iface_addrs(&handle, &conf).await;
    debug!("{:?}", iface_addrs_map);

    apply_hooks(&iface_addrs_map, &conf).await;

    let client = build_http_client();
    let (new_best6, new_best4) = select_best(&iface_addrs_map, conf);
    let mut ok = true;
    if let Some(new_best6_data) = new_best6 {
        ok &= send_ddns_update(&client, IpAddr::V6(new_best6_data), conf)
            .await
            .is_success();
    }
    if let Some(new_best4_data) = new_best4 {
        ok &= send_ddns_update(&client, IpAddr::V4(new_best4_data), conf)
            .await
            .is_success();
    }
    ok
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
    #[arg(short, long, default_value_t = false)]
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
        Some(false) => args.verbose.log_level_filter(),
        Some(true) => log::LevelFilter::Debug,
        None => args.verbose.log_level_filter(),
    };
    let mut builder = env_logger::builder();
    builder
        .format_timestamp(None)
        .format_level(false)
        .format_target(false)
        .filter_level(loglevel);
    // Suppress netlink-packet-route kernel compatibility warnings unless in debug mode
    if loglevel < log::LevelFilter::Debug {
        builder.filter_module(
            "netlink_packet_route::link::buffer_tool",
            log::LevelFilter::Error,
        );
    }
    builder.init();
    info!("Read config file {} finished", &args.config);
    info!("Set log level {:?}", loglevel);

    if let Err(e) = validate_hostname(&conf.hostname) {
        error!(
            "Invalid hostname {:?} in the configuration: {}",
            conf.hostname, e
        );
        std::process::exit(1);
    }

    if args.oneshot {
        info!("Running in oneshot mode...");
        if !oneshot(&conf).await {
            error!("Oneshot DDNS update failed");
            std::process::exit(1);
        }
    } else {
        let mut jset = JoinSet::new();
        let (tx, rx) = mpsc::channel(10);

        jset.spawn(update_worker(rx, conf.clone()));
        jset.spawn(netlink_events_receiver(tx, conf.clone()));

        info!("Listener started, waiting for HUP");
        signal(SignalKind::hangup())
            .expect("Failed to bind to HUP signal.")
            .recv()
            .await;
        info!("Terminating on HUP.");
        jset.shutdown().await;

        let mut output = Vec::new();
        while let Some(res) = jset.join_next().await {
            match res {
                Ok(t) => output.push(t),
                Err(err) if err.is_panic() => panic::resume_unwind(err.into_panic()),
                Err(err) => panic!("{err}"),
            }
        }
    }

    info!("Sucessfully shutdown");
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;

    #[test]
    fn classify_success_keywords() {
        assert!(classify_ddns_response(StatusCode::OK, "good 1.2.3.4").is_success());
        assert!(classify_ddns_response(StatusCode::OK, "nochg 2a02::1").is_success());
        // Case-insensitive keyword matching.
        assert!(classify_ddns_response(StatusCode::OK, "GOOD 1.2.3.4").is_success());
        // Leading blank lines / CRLF are tolerated.
        assert!(classify_ddns_response(StatusCode::OK, "\r\ngood 1.2.3.4\r\n").is_success());
    }

    #[test]
    fn parse_published_addresses() {
        // A dual-stack record set answers with IPv4 first, comma-separated (§7).
        match classify_ddns_response(StatusCode::OK, "good 192.0.2.5,2001:db8::5") {
            DdnsOutcome::Good(addrs) => assert_eq!(
                addrs,
                vec![
                    "192.0.2.5".parse::<IpAddr>().unwrap(),
                    "2001:db8::5".parse::<IpAddr>().unwrap()
                ]
            ),
            other => panic!("expected Good with addresses, got {other:?}"),
        }
        // A bare keyword yields no addresses, and no false mismatch warning.
        match classify_ddns_response(StatusCode::OK, "nochg") {
            DdnsOutcome::NoChg(addrs) => assert!(addrs.is_empty()),
            other => panic!("expected NoChg, got {other:?}"),
        }
    }

    #[test]
    fn classify_permanent_failures() {
        for body in [
            "nohost", "notfqdn", "numhost", "!yours", "badauth", "badagent", "!donator", "!active",
        ] {
            assert!(
                matches!(
                    classify_ddns_response(StatusCode::OK, body),
                    DdnsOutcome::Permanent(_)
                ),
                "expected permanent for body {body:?}"
            );
        }
        // teleddns-server returns non-200 for these; the keyword still decides.
        assert!(matches!(
            classify_ddns_response(StatusCode::UNAUTHORIZED, "badauth"),
            DdnsOutcome::Permanent(_)
        ));
    }

    #[test]
    fn classify_transient_failures() {
        assert!(matches!(
            classify_ddns_response(StatusCode::INTERNAL_SERVER_ERROR, "911"),
            DdnsOutcome::Transient(_)
        ));
        assert!(matches!(
            classify_ddns_response(StatusCode::OK, "dnserr"),
            DdnsOutcome::Transient(_)
        ));
    }

    #[test]
    fn classify_rate_limit_is_retryable_but_slow() {
        // `abuse` is a rate limit (429), not a permanent error: retry, later (§12.3).
        let outcome = classify_ddns_response(StatusCode::TOO_MANY_REQUESTS, "abuse");
        assert!(matches!(outcome, DdnsOutcome::RateLimited(_)));
        assert!(!outcome.is_success());
        assert_eq!(outcome.retry_floor_s(), RETRY_RATELIMIT_MIN_S);
        // Same via the status fallback, for a server with an empty body.
        assert!(matches!(
            classify_ddns_response(StatusCode::TOO_MANY_REQUESTS, ""),
            DdnsOutcome::RateLimited(_)
        ));
    }

    #[test]
    fn classify_unknown_body_falls_back_to_status() {
        // Unknown/empty body with a 2xx is treated as success. This also covers
        // pre-0.4.0 teleddns-server, which answered 200 with a JSON body.
        assert!(classify_ddns_response(StatusCode::OK, "").is_success());
        assert!(classify_ddns_response(StatusCode::OK, "{\"detail\":\"whatever\"}").is_success());
        // 5xx with an unrecognized body -> transient.
        assert!(matches!(
            classify_ddns_response(StatusCode::INTERNAL_SERVER_ERROR, "oops"),
            DdnsOutcome::Transient(_)
        ));
        // Other 4xx with an unrecognized body -> permanent. `forbidden` is what the
        // server's IP allow-list answers with (§1) — not a dyndns2 keyword.
        assert!(matches!(
            classify_ddns_response(StatusCode::BAD_REQUEST, "nope"),
            DdnsOutcome::Permanent(_)
        ));
        assert!(matches!(
            classify_ddns_response(StatusCode::FORBIDDEN, "forbidden"),
            DdnsOutcome::Permanent(_)
        ));
    }

    #[test]
    fn update_url_uses_the_right_parameter_per_family() {
        let base = "https://ddns.example.com/nic/update";
        let v4 = build_update_url(base, "192.0.2.1".parse().unwrap(), "host.example.com").unwrap();
        assert_eq!(
            v4.as_str(),
            "https://ddns.example.com/nic/update?myip=192.0.2.1&hostname=host.example.com"
        );
        // IPv6 must go in `myipv6` — an IPv6 in `myip` is rejected by generic
        // dyndns2 services, and IPv4 in `myipv6` is `notfqdn` (§4).
        let v6 =
            build_update_url(base, "2001:db8::1".parse().unwrap(), "host.example.com").unwrap();
        assert_eq!(
            v6.as_str(),
            "https://ddns.example.com/nic/update?myipv6=2001%3Adb8%3A%3A1&hostname=host.example.com"
        );
        // Existing query parameters in the configured URL are preserved.
        let extra = build_update_url(
            "https://ddns.example.com/update?system=dyndns",
            "192.0.2.1".parse().unwrap(),
            "host.example.com",
        )
        .unwrap();
        assert!(extra
            .as_str()
            .starts_with("https://ddns.example.com/update?system=dyndns&myip=192.0.2.1&hostname="));
    }

    #[test]
    fn hostname_validation() {
        for name in [
            "host.example.com",
            "host.example.com.",
            "*.example.com",
            "_dmarc.example.com",
            "a-b.sub.example.com",
        ] {
            assert!(
                validate_hostname(name).is_ok(),
                "expected {name:?} to be valid"
            );
        }
        for name in [
            "",
            "host",                        // not fully qualified
            "a.example.com,b.example.com", // we send one name per request
            "host .example.com",
            "host..example.com",
            "ho$t.example.com",
            "*.*.example.com", // wildcard only as the first label
        ] {
            assert!(
                validate_hostname(name).is_err(),
                "expected {name:?} to be rejected"
            );
        }
    }

    #[tokio::test]
    async fn permanent_failure_is_not_retried_and_success_is_adopted() {
        // No network here: drive the state machine directly.
        let mut st: PushState<Ipv4Addr> = PushState::default();
        let addr: Ipv4Addr = "192.0.2.1".parse().unwrap();

        st.confirmed = Some(addr);
        assert!(st.is_current(Some(addr)), "an acked address is not re-sent");

        // A permanently rejected address stays blocked until the desired value changes.
        st.confirmed = None;
        st.rejected = Some(addr);
        assert!(!st.is_current(Some(addr)));
        let conf = Config {
            debug: None,
            // An unroutable URL: if the guard failed we would attempt a request and
            // the test would take the HTTP timeout instead of returning at once.
            ddns_url: "http://127.0.0.1:1/nic/update".to_string(),
            hostname: "host.example.com".to_string(),
            enable_ipv6: true,
            enable_ipv4: true,
            report_ipv4_private: false,
            report_ipv6_ula: false,
            interfaces: vec!["*".to_string()],
            hooks: None,
        };
        st.sync(Some(addr), &build_http_client(), &conf).await;
        assert_eq!(st.confirmed, None, "no push happened, nothing to confirm");
        assert_eq!(
            st.backoff_s, 0,
            "a blocked address does not schedule a retry"
        );

        // Losing the address just clears the state — DDNS never deletes a record.
        st.confirmed = Some(addr);
        st.sync(None, &build_http_client(), &conf).await;
        assert_eq!(st.confirmed, None);
    }
}
