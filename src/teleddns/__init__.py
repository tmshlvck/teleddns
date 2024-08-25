#!/usr/bin/env python3
#
# TeleDDNS
# (C) 2015-2024 Tomas Hlavacek (tmshlvck@gmail.com)
#
# This program is free software: you can redistribute it and/or modify
# it under the terms of the GNU General Public License as published by
# the Free Software Foundation, either version 3 of the License, or
# (at your option) any later version.
#
# This program is distributed in the hope that it will be useful,
# but WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
# GNU General Public License for more details.
#
# You should have received a copy of the GNU General Public License
# along with this program.  If not, see <http://www.gnu.org/licenses/>.

import click
import sys
import logging
import yaml
import time
import ipaddress
import requests
import threading
import os
import json

from typing import List, Iterator, Tuple, Union

from pyroute2 import NDB
from pyroute2 import IPRSocket,IPRoute
from pyroute2.netlink.rtnl.ifaddrmsg import IFA_F_MANAGETEMPADDR
from pyroute2.netlink.rtnl.ifinfmsg import IFF_LOWER_UP, IFF_UP, IFF_RUNNING
from pyroute2.netlink.rtnl import RTMGRP_IPV4_IFADDR, RTMGRP_IPV6_IFADDR, RTMGRP_LINK


def get_netlink_addrs() -> Iterator[Tuple[Union[ipaddress.IPv4Address, ipaddress.IPv6Address], str, int, bool]]:
    """ query netlink rtnl for addresses, interfaces and associated flags and state
        return generator of tuples (ipaddress.IPv4|6Address, iface_name: str, ifa_flags: int, is_running: bool)
    """
    with IPRoute() as ipr:
        for a in ipr.get_addr():
            logging.debug(f"netlink: addr={a.get_attr('IFA_ADDRESS')}/{a['prefixlen']} afi={a['family']} ifa_flags={a.get_attr('IFA_FLAGS')}")
            l = ipr.get_links(index=a['index'])[0]
            logging.debug(f"netlink: iface index={a['index']} name={l.get_attr('IFLA_IFNAME')} state={l['state']} opestate={l.get_attr('IFLA_OPERSTATE')}")
            logging.debug(f"netlink: iface index={a['index']} flags {l['flags']} IFF_LOWER_UP={bool(l['flags'] & IFF_LOWER_UP)} IFF_UP={bool(l['flags'] & IFF_UP)} IFF_RUNNING={bool(l['flags'] & IFF_RUNNING)}")
            yield (ipaddress.ip_address(a.get_attr('IFA_ADDRESS')), l.get_attr('IFLA_IFNAME'), a.get_attr('IFA_FLAGS'), True if (l['flags'] & IFF_LOWER_UP) and (l['flags'] & IFF_UP) and (l['flags'] & IFF_RUNNING) else False)

def get_netlink_updates() -> Iterator[Tuple[str,str]]:
    """ Open netlink socket and wait for mesages concerning address addition and removal
        and interfaces going up and down
        return iterator (event: str, ipaddress: str)
    """
    with IPRSocket() as ips:
        ips.bind((RTMGRP_IPV4_IFADDR | RTMGRP_IPV6_IFADDR | RTMGRP_LINK))
        while True:
            for ev in ips.get():
                logging.debug(f"netlink event: {ev.get('event','unknown')} address {ev.get_attr('IFA_ADDRESS')}")
                yield (str(ev.get('event','unknown')), str(ev.get_attr('IFA_ADDRESS')))


def measure_ipv4(ipv4addr, _):
    try:
        ipo = ipaddress.IPv4Address(ipv4addr)
    except:
        return 0

    logging.debug(f"IPv4 address: {str(ipo)}")

    if ipo.is_multicast:
        logging.debug("  -> multicast")
        return 0
    if ipo.is_private:
        logging.debug("  -> private")
        return 0
    if ipo.is_unspecified:
        logging.debug("  -> unspecified")
        return 0
    if ipo.is_reserved:
        logging.debug("  -> reserved")
        return 0
    if ipo.is_loopback:
        logging.debug("  -> loopback")
        return 0
    if ipo.is_link_local:
        logging.debug("  -> link_local")
        return 0

    logging.debug("  -> global_unicast")
    return 1


def measure_ipv6(ipv6addr, ifa_flags):
    def is_eui64(ipa):
        if ipa.packed[11] == 0xff and ipa.packed[12] == 0xfe:
            return True
        else:
            return False

    try:
        ipo = ipaddress.IPv6Address(ipv6addr)
    except:
        return 0

    logging.debug("IPv6 address: %s" % str(ipo))
    if ipo.is_multicast:
        logging.debug("  -> multicast")
        return 0
    if ipo.is_private:
        logging.debug("  -> private")
        return 0
    if ipo.is_unspecified:
        logging.debug("  -> unspecified")
        return 0
    if ipo.is_reserved:
        logging.debug("  -> reserved")
        return 0
    if ipo.is_loopback:
        logging.debug("  -> loopback")
        return 0
    if ipo.is_link_local:
        logging.debug("  -> link_local")
        return 0
    if ipo.is_site_local:
        logging.debug("  -> site_local")
        return 0
    if is_eui64(ipo):
        logging.debug("  -> EUI64")
        return 3
    if ifa_flags & IFA_F_MANAGETEMPADDR: # mngtmpaddr flag
        logging.debug("  -> stable privacy")
        return 2

    logging.debug("  -> global_unicast")
    return 1


def get_host_ipaddr(iface_filter: Union[str, List[str]], enable_ipv4: bool, enable_ipv6: bool):
    """
    iface_filter - None or list of str or str; str = name of interface(s)
    enable_ipv4 - boot = include IPv4 addresses in the result
    enable_ipv6 - bool = include IPv6 addresses in the result
    return ([str IPv4],[str IPv6]), str = IP addresses
    """

    # execute device filter
    def filter_dev(fltr, dev):
        if dev == 'lo':
            return False

        if dev in fltr:
            return True
        if f'-{dev}' in fltr:
            return False
        if '*' in fltr:
            return True

        return False

    # normalize iface_filter
    if iface_filter:
        if type(iface_filter) is str:
            iface_filter = [iface_filter]
    else:
        iface_filter = ['*']

    best_ipv4, best4metric = None, 0
    best_ipv6, best6metric = None, 0
    known_addrs = set()

    for addr,iface,ifa_flags,is_running in get_netlink_addrs():
        known_addrs.add(str(addr))
        if not is_running:
            logging.debug(f"skipping down/not-running iface {iface} and discarding address {addr}")

        logging.debug(f"considering IP address {addr} from interface {iface} with ifa_flags {str(ifa_flags)}")
        if filter_dev(iface_filter, iface):
            logging.debug(f" -> interface {iface} allowed by interface filter")
        else:
            logging.debug(f" -> interface {iface} denied by interface filter")
            continue

        if addr.version == 4 and enable_ipv4:
            m = measure_ipv4(addr, ifa_flags)
            if m > 0 and m > best4metric:
              best_ipv4 = addr
              best4metric = m
        elif addr.version == 6 and enable_ipv6:
            m = measure_ipv6(addr, ifa_flags)
            if m > 0 and m > best6metric:
                best_ipv6 = addr
                best6metric = m

    return (best_ipv4, best_ipv6, known_addrs)


def get_result(response):
    if response.headers.get('Content-Type').startswith('application/json'):
        try:
            return response.json()
        except json.decoder.JSONDecodeError:
            return response.content
    else:
        return response.content

def update_ddns(api_url, hostname, myip):
    logging.debug(f"Executing update with hostname {hostname} and myip {myip}")
    try:
        response = requests.get(api_url, params={'hostname': hostname, 'myip': myip})
        if response.status_code == 200:
            logging.debug(f"Succeeded with response code: {response.status_code} result: {str(get_result(response))}")
            return True
        else:
            logging.error(f"Failed with response code: {response.status_code} result: {str(get_result(response))}")
            return False
    except Exception as e:
        logging.exception("DDNS update failed:")
        return False

def ddns_client(config, oldip4=None, oldip6=None):
    myip4, myip6, known_ipaddrs = get_host_ipaddr(config.get('interfaces', ['*',]), config.get('enable_ipv4', True), config.get('enable_ipv6', True))
    logging.info(f"ddns_client: Selected myip4={myip4} myip6={myip6} with oldip4={oldip4} oldip6={oldip6}")
    if myip4 and myip4 != oldip4:
        if update_ddns(config.get('ddns_url'), config['hostname'], myip4):
            logging.info(f"IPv4 address update sent successfully for {config['hostname']}")
    if myip6 and myip6 != oldip6:
        if update_ddns(config.get('ddns_url'), config['hostname'], myip6):
            logging.info(f"IPv6 address update sent successfully for {config['hostname']}")

    return (myip4, myip6, known_ipaddrs)


known_ipaddrs = set()
ddns_trigger = True
ddns_client_lock = threading.Lock()
def ddns_client_recv_loop():
    global ddns_trigger, known_ipaddrs, ddns_client_lock
    for ev,addr in get_netlink_updates(): # infinite loop
        with ddns_client_lock:
            if ev in ['RTM_DELADDR', 'RTM_NEWLINK', 'RTM_DELLINK']:
                ddns_trigger = True
                logging.debug("trigger set for delete/remove action")
            elif ev == 'RTM_NEWADDR' and not addr in known_ipaddrs:
                ddns_trigger = True
                logging.debug("trigger for new unknown IP")
            else:
                logging.debug("netling event filtered out (address is already known and action is not delete/down)")


def ddns_client_loop(config, min_period=60, force_refresh_period=3600):
    global ddns_trigger, known_ipaddrs, ddns_client_lock

    threading.Thread(target=ddns_client_recv_loop, name="ddns_client_recv_loop", daemon=True).start()
    logging.debug(f"DDNS daemon loop started with period {min_period} sec")
    oldip4, oldip6 = None, None
    since_refresh = 0
    while True:
        with ddns_client_lock:
            if ddns_trigger or since_refresh > force_refresh_period:
                oldip4, oldip6, known_ipaddrs = ddns_client(config, oldip4, oldip6)
                ddns_trigger = False
                since_refresh = 0
        time.sleep(min_period)
        since_refresh += min_period


DEFAULT_CONFIG='/etc/teleddns/teleddns.yaml'

@click.command()
@click.option('-c', '--config', 'config_file', default=os.environ.get('TELEDDNS_CONFIG',DEFAULT_CONFIG),
              help=f"override TELEDDNS_CONFIG env or defult {DEFAULT_CONFIG}")
@click.option('-d', '--debug', 'debug', is_flag=True, help='Enable debugging output.')
@click.option('-n', '--noexit', 'daemon', is_flag=True, help='Run in loop and keep wating for new IPs. Best for systemd simple service.')
@click.option('-h', '--hash', 'hash', help="Hash password and exit.")
def main(config_file, debug, daemon, hash):
    """TeleDDNS client that can become a daemon."""
    
    if hash:
        from passlib.context import CryptContext
        pwd_context = CryptContext(schemes=["bcrypt"], deprecated="auto")
        print(pwd_context.hash(hash))
        return 0

    logcfg = {}
    if daemon: # daemon should run as systemd unit -> timestamps are automatically added when messages get written to journal
        logcfg['format'] = '%(levelname)s %(message)s'
    else:
        logcfg['format'] = '%(asctime)s %(levelname)s %(message)s'
    if debug:
        logcfg['level'] = logging.DEBUG
    else:
        logcfg['level'] = logging.INFO
    logging.basicConfig(**logcfg)

    with open(config_file, 'r') as cfd:
        config = yaml.load(cfd, Loader=yaml.SafeLoader)

    if config.get('logfile', None):
        logcfg['filename'] = config['logfile']
        logging.basicConfig(**logcfg)

    if config.get('debug', False):
        logcfg['level'] = logging.DEBUG
        logging.basicConfig(**logcfg)

    if daemon:
        ddns_client_loop(config)
    else:
        return ddns_client(config)

    return 0

if __name__ == '__main__':
    sys.exit(main())
