/*
TeleDDNS
(C) 2015-2026 Tomas Hlavacek (tmshlvck@gmail.com)

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
use humantime::format_duration;
use log::{debug, error, info, warn};
use netlink_packet_core::NetlinkPayload::InnerMessage;
use netlink_packet_route::address::AddressHeader;
use netlink_packet_route::address::*;
use netlink_packet_route::link::*;
use netlink_packet_route::RouteNetlinkMessage::*;
use netlink_sys::{AsyncSocket, SocketAddr};
use rtnetlink::{new_connection, Error, Handle};
use serde_derive::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::prelude::*;
use std::io::LineWriter;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::panic;
use std::time::SystemTime;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::task::JoinSet;
use tokio::time::Duration;

/// Format a duration for a log line, truncated to whole seconds: sub-second
/// precision is noise for timers on this scale.
fn fmt_dur(d: Duration) -> humantime::FormattedDuration {
    format_duration(Duration::from_secs(d.as_secs()))
}

/// The window the server counts updates in (DYNDNS2.md §10), and therefore the
/// window our own rate budget slides over.
const RATE_WINDOW: Duration = Duration::from_secs(3600);

/// What the server permits per hour and family. We do not use it as a target — the
/// `ddns_max_updates_per_hour` default is half of it — only to warn about a
/// configuration that cannot possibly stay inside the server's limit.
const SERVER_UPDATES_PER_HOUR: u32 = 60;

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
        if let (Some(ifn), true) = (ifname, is_up) {
            res.insert(msg.header.index, ifn);
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
    let iface_map = match nl_get_iface_map(handle).await {
        Ok(res) => res,
        Err(_) => {
            error!("Error while getting interfaces: nl_get_iface_map");
            HashMap::new()
        }
    };
    let addr_vec = match nl_get_addrs(handle).await {
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
                            compute_v6_metric(ip6, iface, &iphdr.flags, conf.report_ipv6_ula)
                        }
                        IpAddr::V4(ip4) => {
                            compute_v4_metric(ip4, iface, &iphdr.flags, conf.report_ipv4_private)
                        }
                    };
                    ifaddrs.insert(
                        IfaceIpAddr {
                            addr: *ip,
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
    if !publish_addrs(&tx, &iface_addrs_map).await {
        return;
    }

    // Optional belt-and-braces re-dump of the whole address set. Off by default:
    // netlink notifications are reliable, and a re-dump that finds nothing new costs
    // one comparison in the worker, not an update.
    let period = conf.ddns_kernel_poll_period;
    let mut poll_tick = if period.is_zero() {
        None
    } else {
        info!("Polling the kernel for addresses every {}", fmt_dur(period));
        Some(tokio::time::interval_at(
            tokio::time::Instant::now() + period,
            period,
        ))
    };

    'events: loop {
        let nlmsg = tokio::select! {
            msg = messages.next() => match msg {
                Some((nlmsg, _)) => nlmsg,
                None => break 'events,
            },
            _ = next_poll_tick(&mut poll_tick) => {
                debug!("Periodic kernel address re-dump");
                iface_addrs_map = get_iface_addrs(&handle, &conf).await;
                if !publish_addrs(&tx, &iface_addrs_map).await {
                    break 'events;
                }
                continue 'events;
            }
        };

        debug!("netlink recv: {:?}", nlmsg.payload);
        match nlmsg.payload {
            InnerMessage(NewAddress(nladdrmsg)) => {
                for attr in nladdrmsg.attributes {
                    if let AddressAttribute::Address(addr) = attr {
                        let received_ifaceaddr = IfaceIpAddr {
                            addr,
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
                            if !publish_addrs(&tx, &iface_addrs_map).await {
                                break 'events;
                            }
                        }
                    }
                }
            }
            _ => {
                debug!("Other type notify -> trigger update");
                iface_addrs_map = get_iface_addrs(&handle, &conf).await;
                debug!("{:?}", iface_addrs_map);
                if !publish_addrs(&tx, &iface_addrs_map).await {
                    break 'events;
                }
            }
        }
    }
}

/// Wait for the next kernel re-dump tick, or forever when polling is disabled.
async fn next_poll_tick(tick: &mut Option<tokio::time::Interval>) {
    match tick {
        Some(interval) => {
            interval.tick().await;
        }
        None => std::future::pending().await,
    }
}

/// Hand the current address set to the update worker. Returns false when the worker
/// is gone, which is the receiver's cue to stop as well.
async fn publish_addrs(
    tx: &Sender<HashMap<IfaceIpAddr, IfaceIpAddrData>>,
    addrs: &HashMap<IfaceIpAddr, IfaceIpAddrData>,
) -> bool {
    match tx.send(addrs.clone()).await {
        Ok(()) => true,
        Err(e) => {
            error!(
                "Update worker is gone, stopping the netlink receiver: {}",
                e
            );
            false
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
    for (ip, ipdata) in state {
        if let IpAddr::V4(ip4) = ipdata.subnet_addr {
            match dedup4.get(&ip4) {
                Some(dedup_pfxlen) if *dedup_pfxlen == ip.prefix_len => {
                    debug!("Deduped {:?}/{}", ip4, dedup_pfxlen);
                    continue;
                }
                _ => {
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
            match write!(wrt, "{}/{}", ip4, ip.prefix_len) {
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
    for (ip, ipdata) in state {
        if let IpAddr::V6(ip6) = ipdata.subnet_addr {
            match dedup6.get(&ip6) {
                Some(dedup_pfxlen) if *dedup_pfxlen == ip.prefix_len => {
                    debug!("Deduped {:?}/{}", ip6, dedup_pfxlen);
                    continue;
                }
                _ => {
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
            match write!(wrt, "{}/{}", ip6, ip.prefix_len) {
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
    /// long wait, see `ddns_retry_ratelimit_min`.
    RateLimited(String),
}

impl DdnsOutcome {
    fn is_success(&self) -> bool {
        matches!(self, DdnsOutcome::Good(_) | DdnsOutcome::NoChg(_))
    }

    /// Shortest acceptable delay before this failure may be retried. Permanent
    /// failures are never retried, so they have no floor.
    fn retry_floor(&self, conf: &Config) -> Duration {
        match self {
            DdnsOutcome::RateLimited(_) => conf.ddns_retry_ratelimit_min,
            _ => conf.ddns_retry_min,
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
fn build_http_client(conf: &Config) -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("teleddns/", env!("CARGO_PKG_VERSION")))
        .timeout(conf.http_timeout)
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

/// Client-side rate governor. teleddns-server allows 60 updates per hour and
/// address family (DYNDNS2.md §10) and answers `abuse` past that, so we keep our own
/// budget comfortably below it: a flapping address, a retry loop or a restart loop
/// must never be able to get us rate limited. The budget only ever *delays* a
/// request — the pending address is always the latest one, so a burst collapses
/// into a single update carrying the final value.
#[derive(Debug)]
struct RateBudget {
    min_interval: Duration,
    max_per_window: u32,
    /// When we sent, oldest first, pruned to `RATE_WINDOW`.
    sent: VecDeque<SystemTime>,
}

impl RateBudget {
    fn new(conf: &Config) -> Self {
        RateBudget {
            min_interval: conf.ddns_min_interval,
            max_per_window: conf.ddns_max_updates_per_hour,
            sent: VecDeque::new(),
        }
    }

    fn prune(&mut self, now: SystemTime) {
        while let Some(oldest) = self.sent.front() {
            match now.duration_since(*oldest) {
                Ok(age) if age >= RATE_WINDOW => {
                    self.sent.pop_front();
                }
                _ => break,
            }
        }
    }

    /// `None` when a request may go out now, `Some(wait)` when it may not.
    fn check(&mut self, now: SystemTime) -> Option<Duration> {
        self.prune(now);
        if let Some(last) = self.sent.back() {
            let since = now.duration_since(*last).unwrap_or(Duration::ZERO);
            if let Some(wait) = self.min_interval.checked_sub(since) {
                if !wait.is_zero() {
                    return Some(wait);
                }
            }
        }
        if self.sent.len() as u32 >= self.max_per_window {
            // Wait for the oldest send to age out of the window. Never zero, so a
            // caller that keeps asking cannot spin.
            let wait = match self.sent.front() {
                Some(oldest) => {
                    let age = now.duration_since(*oldest).unwrap_or(Duration::ZERO);
                    RATE_WINDOW.saturating_sub(age)
                }
                None => RATE_WINDOW,
            };
            return Some(wait.max(Duration::from_secs(1)));
        }
        None
    }

    fn record(&mut self, now: SystemTime) {
        self.sent.push_back(now);
        self.prune(now);
    }
}

/// Whether the unconditional re-publish (§6 — the server may have lost or changed
/// the record without us noticing) is due.
#[derive(Debug, PartialEq)]
enum Resync {
    Disabled,
    Due,
    After(Duration),
}

impl Resync {
    /// How long until this family wants attention again on account of the resync.
    fn wake(&self) -> Option<Duration> {
        match self {
            Resync::After(d) => Some(*d),
            // `Due` is only ever observed by a caller that is about to act on it.
            Resync::Due => Some(Duration::ZERO),
            Resync::Disabled => None,
        }
    }
}

/// Per-family push state and pacing. Implements DYNDNS2.md §12.3: `good`/`nochg` are
/// adopted and not re-sent, transient failures back off exponentially, `abuse` backs
/// off on an hour scale, and a permanent failure is not retried until the address,
/// the configuration or the resync tick says otherwise — all of it on top of the
/// rate budget, which no request bypasses.
#[derive(Debug)]
struct PushState<A> {
    /// Last value the server acknowledged.
    confirmed: Option<A>,
    /// Value the server rejected permanently — do not send it again.
    rejected: Option<A>,
    /// Last time we sent anything for this family; drives the resync tick.
    last_attempt: Option<SystemTime>,
    /// Earliest time a retry may be attempted after a transient failure.
    next_retry: Option<SystemTime>,
    /// Current back-off; zero until the first transient failure.
    backoff: Duration,
    budget: RateBudget,
}

impl<A> PushState<A> {
    fn new(conf: &Config) -> Self {
        PushState {
            confirmed: None,
            rejected: None,
            last_attempt: None,
            next_retry: None,
            backoff: Duration::ZERO,
            budget: RateBudget::new(conf),
        }
    }

    /// Time left until the unconditional re-publish. Counted from the last *attempt*,
    /// not the last success: that way a name the server keeps refusing is retried
    /// once per period instead of on every wake-up.
    fn resync(&self, now: SystemTime, conf: &Config) -> Resync {
        if conf.ddns_resync_period.is_zero() {
            return Resync::Disabled;
        }
        let Some(last) = self.last_attempt else {
            return Resync::Due;
        };
        // A clock that moved backwards (no RTC, pre-NTP boot) counts as due.
        let elapsed = now.duration_since(last).unwrap_or(conf.ddns_resync_period);
        match conf.ddns_resync_period.checked_sub(elapsed) {
            Some(left) if !left.is_zero() => Resync::After(left),
            _ => Resync::Due,
        }
    }
}

impl<A: Copy + PartialEq + Into<IpAddr> + std::fmt::Display> PushState<A> {
    /// Bring the server in line with `desired`. `forced` (a control-socket
    /// `send_update`, §8) skips the back-off, but never the rate budget.
    ///
    /// Returns how long until this family wants to be looked at again — a pending
    /// retry, a rate-budget wait or the next resync — or `None` when nothing is
    /// scheduled and only a new address can change anything.
    async fn sync(
        &mut self,
        desired: Option<A>,
        forced: bool,
        client: &reqwest::Client,
        conf: &Config,
    ) -> Option<Duration> {
        let now = SystemTime::now();
        let Some(addr) = desired else {
            // Losing an address is not an update: the DDNS endpoint never deletes a
            // record set (§6). Forget what we published and wait for a new address.
            if self.confirmed.is_some() {
                info!("No eligible address left to publish; leaving the record as it is");
            }
            self.confirmed = None;
            self.clear_retry();
            return None;
        };

        let resync = self.resync(now, conf);
        let due_resync = resync == Resync::Due;
        let changed = self.confirmed != Some(addr);

        if !changed && !forced && !due_resync {
            return resync.wake();
        }
        if self.rejected == Some(addr) && !forced && !due_resync {
            debug!(
                "Not re-sending {}: the server rejected it permanently",
                addr
            );
            return resync.wake();
        }
        if !forced {
            if let Some(next) = self.next_retry {
                if let Ok(wait) = next.duration_since(now) {
                    if !wait.is_zero() {
                        debug!(
                            "Holding back the update of {} for {}: retry back-off",
                            addr,
                            fmt_dur(wait)
                        );
                        return Some(wait);
                    }
                }
            }
        }
        if let Some(wait) = self.budget.check(now) {
            warn!(
                "Rate budget: holding back the update of {} for {} \
                 (at most {} updates/hour, at least {} apart, per family)",
                addr,
                fmt_dur(wait),
                conf.ddns_max_updates_per_hour,
                fmt_dur(conf.ddns_min_interval)
            );
            return Some(wait);
        }

        if !changed {
            info!("Periodic resync of {}", addr);
        }
        self.budget.record(now);
        self.last_attempt = Some(now);
        let outcome = send_ddns_update(client, addr.into(), conf).await;
        // The request itself took time; do not credit it to the back-off.
        let now = SystemTime::now();

        if outcome.is_success() {
            self.confirmed = Some(addr);
            self.rejected = None;
            self.clear_retry();
            return self.resync(now, conf).wake();
        }
        if let DdnsOutcome::Permanent(_) = outcome {
            self.rejected = Some(addr);
            self.clear_retry();
            // The next resync tick gives the operator a periodic retry, which is how
            // a fixed grant or a created zone eventually gets picked up.
            return self.resync(now, conf).wake();
        }
        let delay = self
            .backoff
            .max(outcome.retry_floor(conf))
            .min(conf.ddns_retry_max);
        self.next_retry = Some(now + delay);
        self.backoff = (delay * 2).min(conf.ddns_retry_max);
        warn!("Retrying the update of {} in {}", addr, fmt_dur(delay));
        Some(delay)
    }

    fn clear_retry(&mut self) {
        self.next_retry = None;
        self.backoff = Duration::ZERO;
    }
}

/// Sleep until `deadline`, or forever when there is nothing scheduled.
async fn sleep_until_opt(deadline: Option<SystemTime>) {
    match deadline {
        Some(t) => {
            let d = t
                .duration_since(SystemTime::now())
                .unwrap_or(Duration::ZERO);
            tokio::time::sleep(d).await
        }
        None => std::future::pending().await,
    }
}

async fn update_worker(mut rx: Receiver<HashMap<IfaceIpAddr, IfaceIpAddrData>>, conf: Config) {
    let client = build_http_client(&conf);
    let mut state4: PushState<Ipv4Addr> = PushState::new(&conf);
    let mut state6: PushState<Ipv6Addr> = PushState::new(&conf);

    let mut current_addrs: Option<HashMap<IfaceIpAddr, IfaceIpAddrData>> = None;
    let (mut desired4, mut desired6): (Option<Ipv4Addr>, Option<Ipv6Addr>) = (None, None);
    // Debounce window: set when the best address changes, cleared when it expires.
    let mut settle: Option<SystemTime> = None;
    // Next time a family wants attention (retry, rate budget, resync).
    let mut wake_at: Option<SystemTime> = None;

    loop {
        // While the settle window is open we wait for it and nothing else: a due
        // retry is at most `ddns_settle_delay` late, and we would rather send once
        // with the final address than twice around a change.
        let deadline = match settle {
            Some(_) => settle,
            None => wake_at,
        };
        tokio::select! {
            msg = rx.recv() => {
                let Some(addrs) = msg else {
                    info!("Channel closed, stopping update worker!");
                    break;
                };
                if current_addrs.as_ref() == Some(&addrs) {
                    debug!("Address state unchanged -> nothing to do");
                    continue;
                }
                info!("Detected address state change");
                apply_hooks(&addrs, &conf).await;
                let (best6, best4) = select_best(&addrs, &conf);
                current_addrs = Some(addrs);
                if (best4, best6) != (desired4, desired6) {
                    info!("Best addresses to publish: v4={:?} v6={:?}", best4, best6);
                    (desired4, desired6) = (best4, best6);
                    // Arm the debounce window once and do not slide it: a flapping
                    // address would otherwise postpone the update indefinitely.
                    if settle.is_none() {
                        settle = Some(SystemTime::now() + conf.ddns_settle_delay);
                    }
                }
                continue;
            }
            _ = sleep_until_opt(deadline) => {}
        }

        settle = None;
        let wake6 = state6.sync(desired6, false, &client, &conf).await;
        let wake4 = state4.sync(desired4, false, &client, &conf).await;
        let next = [wake4, wake6].into_iter().flatten().min();
        wake_at = next.map(|d| SystemTime::now() + d);
        match next {
            Some(d) => debug!("Next scheduled attempt in {}", fmt_dur(d)),
            None => debug!("Nothing scheduled; waiting for an address change"),
        }
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

    let iface_addrs_map = get_iface_addrs(&handle, conf).await;
    debug!("{:?}", iface_addrs_map);

    apply_hooks(&iface_addrs_map, conf).await;

    let client = build_http_client(conf);
    let (best6, best4) = select_best(&iface_addrs_map, conf);

    // `--oneshot` is an operator action: force the update through and report what
    // happened. It starts from empty state, so there is nothing to hold it back —
    // no debounce, no back-off, nothing previously confirmed to compare against.
    // Continuous operation is the daemon's job; see the README.
    let mut state4: PushState<Ipv4Addr> = PushState::new(conf);
    let mut state6: PushState<Ipv6Addr> = PushState::new(conf);
    state6.sync(best6, true, &client, conf).await;
    state4.sync(best4, true, &client, conf).await;

    state4.confirmed == best4 && state6.confirmed == best6
}

#[derive(Debug, PartialEq, Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
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

/// Debounce window: how long to wait for the address set to stop changing before
/// publishing. Long enough to collapse a link bounce into one update.
fn default_settle_delay() -> Duration {
    Duration::from_secs(5)
}

/// Hard minimum between two updates of the same family. The server allows 60 per
/// hour and family (DYNDNS2.md §10), i.e. one per minute sustained.
fn default_min_interval() -> Duration {
    Duration::from_secs(60)
}

/// Sliding-window cap per family, deliberately at half the server's 60/hour so we
/// hit our own governor first and never learn about the limit from an `abuse`.
fn default_max_updates_per_hour() -> u32 {
    30
}

fn default_retry_min() -> Duration {
    Duration::from_secs(60)
}

fn default_retry_max() -> Duration {
    Duration::from_secs(3600)
}

/// Back-off floor after `abuse`/429. The server's counter is a one-hour window, so
/// a minute-scale retry would only refresh it.
fn default_retry_ratelimit_min() -> Duration {
    Duration::from_secs(900)
}

/// Unconditional re-publish interval. Cheap insurance against a record that was
/// changed or lost server-side: 2 requests per family per day.
fn default_resync_period() -> Duration {
    Duration::from_secs(43200)
}

/// Periodic full address re-dump from the kernel. Off by default — netlink
/// notifications are reliable; this exists for the paranoid.
fn default_kernel_poll_period() -> Duration {
    Duration::ZERO
}

fn default_http_timeout() -> Duration {
    Duration::from_secs(30)
}

/// Unknown keys are refused rather than ignored: the config is usually generated
/// (deploy.sh, packaging, config management), so a stale or misspelled key is a
/// mistake worth reporting at startup instead of silently taking the default.
#[derive(Debug, PartialEq, Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
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

    // Timing. Durations are written as human-readable strings ("5s", "15min",
    // "12h"), matching teleddns-server's convention.
    #[serde(default = "default_settle_delay", with = "humantime_serde")]
    ddns_settle_delay: Duration,
    #[serde(default = "default_min_interval", with = "humantime_serde")]
    ddns_min_interval: Duration,
    #[serde(default = "default_max_updates_per_hour")]
    ddns_max_updates_per_hour: u32,
    #[serde(default = "default_retry_min", with = "humantime_serde")]
    ddns_retry_min: Duration,
    #[serde(default = "default_retry_max", with = "humantime_serde")]
    ddns_retry_max: Duration,
    #[serde(default = "default_retry_ratelimit_min", with = "humantime_serde")]
    ddns_retry_ratelimit_min: Duration,
    // The aliases are the names the released 0.3.x config used for the same
    // settings. They are accepted so an upgrade does not refuse to start on a config
    // that tuned them -- a rename is not a typo. `convert-config` (§7) will rewrite
    // them, after which the aliases can go.
    #[serde(
        default = "default_resync_period",
        with = "humantime_serde",
        alias = "resync_interval"
    )]
    ddns_resync_period: Duration,
    #[serde(
        default = "default_kernel_poll_period",
        with = "humantime_serde",
        alias = "redump_interval"
    )]
    ddns_kernel_poll_period: Duration,
    #[serde(default = "default_http_timeout", with = "humantime_serde")]
    http_timeout: Duration,
}

/// Startup sanity checks. Everything that would otherwise only show up as a server
/// error, a stuck timer or a division by zero at the first update.
fn validate_config(conf: &Config) -> Result<(), String> {
    validate_hostname(&conf.hostname)
        .map_err(|e| format!("hostname {:?}: {}", conf.hostname, e))?;
    // Not echoing the URL back: it may carry the password, and this message goes to
    // the log.
    let url = reqwest::Url::parse(&conf.ddns_url).map_err(|e| format!("ddns_url: {e}"))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(format!(
            "ddns_url must be http or https, not {:?}",
            url.scheme()
        ));
    }
    if conf.ddns_max_updates_per_hour == 0 {
        return Err("ddns_max_updates_per_hour must be at least 1".to_string());
    }
    if conf.ddns_retry_min > conf.ddns_retry_max {
        return Err(format!(
            "ddns_retry_min ({}) must not exceed ddns_retry_max ({})",
            fmt_dur(conf.ddns_retry_min),
            fmt_dur(conf.ddns_retry_max)
        ));
    }
    if conf.http_timeout.is_zero() {
        return Err("http_timeout must not be zero".to_string());
    }
    // The server allows 60 updates per hour and family and answers `abuse` past that
    // (DYNDNS2.md §10). A budget above that guarantees we find out the hard way.
    if conf.ddns_max_updates_per_hour > SERVER_UPDATES_PER_HOUR {
        warn!(
            "ddns_max_updates_per_hour is {}, above the {} per hour and family that \
             teleddns-server permits — expect `abuse` responses",
            conf.ddns_max_updates_per_hour, SERVER_UPDATES_PER_HOUR
        );
    }
    if conf.ddns_min_interval < Duration::from_secs(60) {
        warn!(
            "ddns_min_interval is {}, below the one update per minute and family that \
             teleddns-server sustains — expect `abuse` responses",
            fmt_dur(conf.ddns_min_interval)
        );
    }
    if !conf.ddns_resync_period.is_zero() && conf.ddns_resync_period < Duration::from_secs(3600) {
        warn!(
            "ddns_resync_period is {}; a short resync period spends the update budget \
             for nothing — 12h is plenty",
            fmt_dur(conf.ddns_resync_period)
        );
    }
    Ok(())
}

/// Wait for whichever termination signal arrives first; returns its name.
async fn wait_for_termination() -> Result<&'static str, std::io::Error> {
    let mut sighup = signal(SignalKind::hangup())?;
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;
    Ok(tokio::select! {
        _ = sighup.recv() => "SIGHUP",
        _ = sigterm.recv() => "SIGTERM",
        _ = sigint.recv() => "SIGINT",
    })
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

/// Read and parse the config. A bad config is the operator's to fix, not a crash, so
/// the caller reports it as a message: with `deny_unknown_fields` a stale or
/// misspelled key ends up here, and serde names both the key and the line.
fn load_config(path: &str) -> Result<Config, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    serde_yaml::from_reader(file).map_err(|e| e.to_string())
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let conf = match load_config(&args.config) {
        Ok(conf) => conf,
        Err(e) => {
            // The logger is not up yet, so this goes straight to stderr.
            eprintln!("teleddns: {}: {}", args.config, e);
            std::process::exit(1);
        }
    };

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
    info!("Read config file {} finished", args.config);
    info!("Set log level {:?}", loglevel);

    if let Err(e) = validate_config(&conf) {
        error!("Invalid configuration: {}", e);
        std::process::exit(1);
    }

    if args.oneshot {
        info!("Running in oneshot mode...");
        if !oneshot(&conf).await {
            error!("Oneshot DDNS update did not complete — see the log above");
            std::process::exit(1);
        }
    } else {
        let mut jset = JoinSet::new();
        let (tx, rx) = mpsc::channel(10);

        jset.spawn(update_worker(rx, conf.clone()));
        jset.spawn(netlink_events_receiver(tx, conf.clone()));

        // systemd sends SIGTERM on `systemctl stop`, a terminal SIGINT on Ctrl-C;
        // handling only SIGHUP turned both into a hard kill.
        info!("Listener started, waiting for a termination signal");
        match wait_for_termination().await {
            Ok(sig) => info!("Terminating on {}.", sig),
            Err(e) => {
                error!("Failed to install the signal handlers: {}", e);
                std::process::exit(1);
            }
        }
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
        let conf = test_config();
        let outcome = classify_ddns_response(StatusCode::TOO_MANY_REQUESTS, "abuse");
        assert!(matches!(outcome, DdnsOutcome::RateLimited(_)));
        assert!(!outcome.is_success());
        assert_eq!(outcome.retry_floor(&conf), conf.ddns_retry_ratelimit_min);
        assert!(outcome.retry_floor(&conf) > conf.ddns_retry_min);
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

    /// A config whose `ddns_url` refuses the connection instantly (nothing listens on
    /// port 1), so the tests exercise the failure paths without a server and without
    /// waiting for a timeout.
    fn test_config() -> Config {
        Config {
            debug: None,
            ddns_url: "http://127.0.0.1:1/nic/update".to_string(),
            hostname: "host.example.com".to_string(),
            enable_ipv6: true,
            enable_ipv4: true,
            report_ipv4_private: false,
            report_ipv6_ula: false,
            interfaces: vec!["*".to_string()],
            hooks: None,
            ddns_settle_delay: default_settle_delay(),
            ddns_min_interval: default_min_interval(),
            ddns_max_updates_per_hour: default_max_updates_per_hour(),
            ddns_retry_min: default_retry_min(),
            ddns_retry_max: default_retry_max(),
            ddns_retry_ratelimit_min: default_retry_ratelimit_min(),
            ddns_resync_period: default_resync_period(),
            ddns_kernel_poll_period: default_kernel_poll_period(),
            http_timeout: default_http_timeout(),
        }
    }

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    /// An arbitrary fixed point in time; the budget only ever looks at differences.
    fn t0() -> SystemTime {
        SystemTime::UNIX_EPOCH + secs(1_000_000_000)
    }

    #[test]
    fn rate_budget_enforces_the_minimum_interval() {
        let mut conf = test_config();
        conf.ddns_min_interval = secs(60);
        let mut budget = RateBudget::new(&conf);

        assert_eq!(
            budget.check(t0()),
            None,
            "the first update goes out at once"
        );
        budget.record(t0());
        assert_eq!(budget.check(t0() + secs(30)), Some(secs(30)));
        assert_eq!(budget.check(t0() + secs(59)), Some(secs(1)));
        assert_eq!(budget.check(t0() + secs(60)), None);
    }

    #[test]
    fn rate_budget_enforces_the_hourly_cap() {
        let mut conf = test_config();
        conf.ddns_min_interval = Duration::ZERO;
        conf.ddns_max_updates_per_hour = 3;
        let mut budget = RateBudget::new(&conf);

        for i in 0..3 {
            let now = t0() + secs(i * 600);
            assert_eq!(budget.check(now), None);
            budget.record(now);
        }
        // The window is full: wait for the oldest send to age out of the hour.
        assert_eq!(budget.check(t0() + secs(1800)), Some(secs(1800)));
        assert_eq!(budget.check(t0() + secs(3600)), None);
    }

    #[test]
    fn rate_budget_caps_a_flapping_address() {
        // The scenario the governor exists for: an address that changes every second
        // for two hours (a rotating privacy address, a bouncing link).
        let conf = test_config();
        let mut budget = RateBudget::new(&conf);
        let mut sent: Vec<u64> = Vec::new();
        for s in 0..7200 {
            let now = t0() + secs(s);
            if budget.check(now).is_none() {
                budget.record(now);
                sent.push(s);
            }
        }

        assert!(!sent.is_empty(), "the governor must not block everything");
        // Never two updates closer than the minimum interval...
        for pair in sent.windows(2) {
            assert!(
                pair[1] - pair[0] >= conf.ddns_min_interval.as_secs(),
                "updates at {} and {} are too close",
                pair[0],
                pair[1]
            );
        }
        // ...and never more than the cap within any one-hour window, which is what
        // keeps us clear of the server's 60/hour (DYNDNS2.md §10).
        let cap = conf.ddns_max_updates_per_hour as usize;
        assert!(cap < SERVER_UPDATES_PER_HOUR as usize);
        for (i, start) in sent.iter().enumerate() {
            let in_window = sent[i..].iter().take_while(|s| **s - start < 3600).count();
            assert!(
                in_window <= cap,
                "{} updates within the hour starting at {}",
                in_window,
                start
            );
        }
    }

    #[test]
    fn resync_is_due_only_after_the_period() {
        let mut conf = test_config();
        conf.ddns_resync_period = secs(3600);
        let mut st: PushState<Ipv4Addr> = PushState::new(&conf);

        // Nothing sent yet — the first update is always due.
        assert_eq!(st.resync(t0(), &conf), Resync::Due);
        st.last_attempt = Some(t0());
        assert_eq!(
            st.resync(t0() + secs(600), &conf),
            Resync::After(secs(3000))
        );
        assert_eq!(st.resync(t0() + secs(3600), &conf), Resync::Due);
        // A clock that moved backwards (no RTC, pre-NTP boot) counts as due rather
        // than stalling the resync until the clock catches up.
        assert_eq!(st.resync(t0() - secs(10), &conf), Resync::Due);

        conf.ddns_resync_period = Duration::ZERO;
        assert_eq!(st.resync(t0(), &conf), Resync::Disabled);
        assert_eq!(st.resync(t0(), &conf).wake(), None);
    }

    #[tokio::test]
    async fn transient_failures_back_off_exponentially() {
        let mut conf = test_config();
        // Not what this test is about; the budget has its own tests.
        conf.ddns_min_interval = Duration::ZERO;
        conf.ddns_retry_min = secs(60);
        conf.ddns_retry_max = secs(240);
        let client = build_http_client(&conf);
        let mut st: PushState<Ipv4Addr> = PushState::new(&conf);
        let addr: Ipv4Addr = "192.0.2.1".parse().unwrap();

        // Connection refused is a transient failure: retry, doubling up to the ceiling.
        let mut delays = Vec::new();
        for _ in 0..4 {
            delays.push(st.sync(Some(addr), false, &client, &conf).await);
            // Pretend the back-off elapsed, so the next call attempts a push.
            st.next_retry = None;
        }
        assert_eq!(
            delays,
            vec![
                Some(secs(60)),
                Some(secs(120)),
                Some(secs(240)),
                Some(secs(240))
            ]
        );
        assert_eq!(st.confirmed, None, "a failed push confirms nothing");

        // The back-off holds a retry back until it elapses, and reports the remainder.
        st.next_retry = Some(SystemTime::now() + secs(120));
        let wait = st.sync(Some(addr), false, &client, &conf).await.unwrap();
        assert!(wait <= secs(120) && wait > secs(110), "wait was {wait:?}");
        // A forced push (control socket, §8) ignores the back-off.
        st.sync(Some(addr), true, &client, &conf).await;
        assert!(st.last_attempt.is_some());
    }

    #[tokio::test]
    async fn permanent_failure_is_not_retried_until_the_resync() {
        let mut conf = test_config();
        conf.ddns_resync_period = secs(3600);
        let client = build_http_client(&conf);
        let mut st: PushState<Ipv4Addr> = PushState::new(&conf);
        let addr: Ipv4Addr = "192.0.2.1".parse().unwrap();

        // Simulate a `badauth`/`!yours` answer for this address.
        st.rejected = Some(addr);
        st.last_attempt = Some(SystemTime::now());
        let wake = st.sync(Some(addr), false, &client, &conf).await;
        assert_eq!(st.confirmed, None, "no push happened, nothing to confirm");
        assert_eq!(st.backoff, Duration::ZERO, "no retry was scheduled");
        assert!(
            wake.is_some_and(|d| d > secs(3500)),
            "the next attempt is the resync tick, not a retry: {wake:?}"
        );

        // A different address is not covered by the rejection and is attempted.
        let other: Ipv4Addr = "192.0.2.2".parse().unwrap();
        st.sync(Some(other), false, &client, &conf).await;
        assert!(st.backoff > Duration::ZERO, "the new address was attempted");
    }

    #[tokio::test]
    async fn an_acked_address_is_not_resent() {
        let conf = test_config();
        let client = build_http_client(&conf);
        let mut st: PushState<Ipv4Addr> = PushState::new(&conf);
        let addr: Ipv4Addr = "192.0.2.1".parse().unwrap();

        st.confirmed = Some(addr);
        st.last_attempt = Some(SystemTime::now());
        let wake = st.sync(Some(addr), false, &client, &conf).await;
        assert_eq!(st.backoff, Duration::ZERO, "nothing was sent");
        assert!(
            wake.is_some_and(|d| d > conf.ddns_resync_period - secs(60)),
            "the only thing left to do is the resync: {wake:?}"
        );

        // Losing the address just clears our state — DDNS never deletes a record (§6).
        assert_eq!(st.sync(None, false, &client, &conf).await, None);
        assert_eq!(st.confirmed, None);
    }

    #[test]
    fn timing_config_parses_human_durations() {
        let conf: Config = serde_yaml::from_str(
            r#"
ddns_url: "https://ddns.example.com/nic/update"
hostname: "host.example.com"
enable_ipv6: true
enable_ipv4: true
interfaces: ["*"]
ddns_settle_delay: "10s"
ddns_min_interval: "2min"
ddns_resync_period: "6h"
"#,
        )
        .expect("config");
        assert_eq!(conf.ddns_settle_delay, secs(10));
        assert_eq!(conf.ddns_min_interval, secs(120));
        assert_eq!(conf.ddns_resync_period, secs(6 * 3600));
        // Unset keys keep their defaults.
        assert_eq!(conf.ddns_retry_max, default_retry_max());
        assert!(validate_config(&conf).is_ok());
    }

    #[test]
    fn a_config_without_any_timing_keys_still_loads() {
        // Every 0.3.x config must keep working: the timing keys are all optional.
        let conf: Config = serde_yaml::from_str(
            r#"
ddns_url: "https://user:pass@ddns.example.com/nic/update"
hostname: "host.example.com"
enable_ipv6: true
enable_ipv4: false
interfaces: ["*", "-wg0"]
hooks:
  - shell: "true"
"#,
        )
        .expect("legacy config");
        assert_eq!(conf.ddns_settle_delay, default_settle_delay());
        assert_eq!(conf.ddns_min_interval, default_min_interval());
        assert_eq!(conf.http_timeout, default_http_timeout());
        assert!(validate_config(&conf).is_ok());
    }

    /// Uncomment the `# key: value` settings of the sample config, leaving prose and
    /// the commented hooks block (which has no value on the key line) alone.
    fn uncomment_settings(src: &str) -> String {
        src.lines()
            .map(|line| match line.strip_prefix("# ") {
                Some(rest) => {
                    let is_setting = rest.split_once(": ").is_some_and(|(key, value)| {
                        !key.is_empty()
                            && key
                                .chars()
                                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                            && !value.trim().is_empty()
                    });
                    if is_setting {
                        rest.to_string()
                    } else {
                        line.to_string()
                    }
                }
                None => line.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_shipped_sample_config_parses() {
        let sample = include_str!("../teleddns.yaml.sample");
        let conf: Config = serde_yaml::from_str(sample).expect("the shipped sample must parse");
        assert!(validate_config(&conf).is_ok());

        // Now with every documented key switched on. `deny_unknown_fields` turns a
        // stale key in the sample into a broken config for whoever uncomments it, so
        // the sample has to be checked, not just trusted.
        let all_keys = uncomment_settings(sample);
        let conf: Config = serde_yaml::from_str(&all_keys)
            .expect("every key documented in the sample must be accepted");
        assert_eq!(conf.ddns_resync_period, secs(12 * 3600));
        assert_eq!(conf.ddns_max_updates_per_hour, 30);
        assert!(validate_config(&conf).is_ok());
    }

    #[test]
    fn a_released_0_3_x_config_still_loads() {
        // The keys and value formats of the last released (golang) config, including
        // the two timing keys that were renamed. Refusing to start on these would
        // break an unattended upgrade of a client whose whole job is keeping DNS live.
        let conf: Config = serde_yaml::from_str(
            r#"
debug: false
ddns_url: "https://user:pass@ddns.example.com/ddns/update"
hostname: "host.example.com"
enable_ipv6: true
enable_ipv4: false
report_ipv4_private: false
report_ipv6_ula: false
resync_interval: 24h
redump_interval: 15m
interfaces:
  - '*'
hooks:
  - nft_sets_outfile: "/etc/nftables.d/00-localnets.rules"
    shell: "nft -f /etc/nftables.conf"
"#,
        )
        .expect("a released 0.3.x config must still load");
        assert_eq!(conf.ddns_resync_period, secs(24 * 3600));
        assert_eq!(conf.ddns_kernel_poll_period, secs(15 * 60));
        assert!(validate_config(&conf).is_ok());
    }

    #[test]
    fn a_misspelled_config_key_is_rejected() {
        let base = r#"
ddns_url: "https://ddns.example.com/nic/update"
hostname: "host.example.com"
enable_ipv6: true
enable_ipv4: true
interfaces: ["*"]
"#;
        let err = serde_yaml::from_str::<Config>(&format!("{base}ddns_min_intervall: '60s'\n"))
            .expect_err("a misspelled key must not be silently ignored");
        assert!(
            err.to_string().contains("ddns_min_intervall"),
            "the error must name the offending key: {err}"
        );

        // A typo inside a hook would silently disable the hook, which is worse.
        assert!(
            serde_yaml::from_str::<Config>(&format!("{base}hooks:\n  - shel: 'true'\n")).is_err(),
            "an unknown hook key must be rejected too"
        );
    }

    #[test]
    fn config_validation_rejects_unusable_timing() {
        let mut conf = test_config();
        conf.ddns_max_updates_per_hour = 0;
        assert!(validate_config(&conf).is_err());

        let mut conf = test_config();
        conf.ddns_retry_min = secs(600);
        conf.ddns_retry_max = secs(60);
        assert!(validate_config(&conf).is_err());

        let mut conf = test_config();
        conf.http_timeout = Duration::ZERO;
        assert!(validate_config(&conf).is_err());

        let mut conf = test_config();
        conf.ddns_url = "ftp://ddns.example.com/update".to_string();
        assert!(validate_config(&conf).is_err());

        let mut conf = test_config();
        conf.ddns_url = "not a url".to_string();
        assert!(validate_config(&conf).is_err());

        // Above the server's limit is a warning, not an error: another server may
        // allow more.
        let mut conf = test_config();
        conf.ddns_max_updates_per_hour = 600;
        conf.ddns_min_interval = secs(1);
        assert!(validate_config(&conf).is_ok());
    }
}
