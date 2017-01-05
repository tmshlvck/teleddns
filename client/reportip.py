#!/usr/bin/env python
#
# DDNS
# (C) 2015-2017 Tomas Hlavacek (tmshlvck@gmail.com)
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


import sys
import getopt
import os
import re
import httplib2
import json
import syslog
import subprocess
import re
import ipaddr
import time

sys.path.insert(0, '/etc/ddns')
from reportipconf import *


def d(message):
        if debug:
                print(message)

def log(message):
        if debug:
                print(message)
                syslog.syslog(message)
        else:
                syslog.syslog(message)


ipv4regexp=re.compile(r'^\s+inet\s+(([0-9]{1,3}\.){3}[0-9]{1,3})/[0-9]{1,2}\s+')
ipv6regexp=re.compile(r'^\s+inet6\s+(([0-9a-fA-F]{0,4}:){0,7}[0-9a-fA-F]{0,4})/[0-9]{1,3}\s+')
def get_ipaddr(dev):
        p=subprocess.Popen([bin_ip, 'address', 'show', 'dev', dev],stdout=subprocess.PIPE)
        r=p.communicate()

        ipv4 = []
        ipv6 = []
        if r[0]:
                for l in r[0].split('\n'):
                        m = ipv4regexp.match(l)
                        if m:
                                ipv4.append(m.group(1))

                        m = ipv6regexp.match(l)
                        if m:
                                ipv6.append(m.group(1))
        return (ipv4,ipv6)


def select_ipv4(ipv4list):
        if ipv4list:
                for ip in ipv4list:
                        ipo = ipaddr.IPAddress(ip)
                        if ipo.is_multicast:
                                continue
                        if ipo.is_private:
                                continue
                        if ipo.is_unspecified:
                                continue
                        if ipo.is_reserved:
                                continue
                        if ipo.is_loopback:
                                continue
                        if ipo.is_link_local:
                                continue

                        return ip

def select_ipv6(ipv6list):        
        if ipv6list:
                noneui64=None
                for ip in ipv6list:
                        ipo = ipaddr.IPAddress(ip)
                        if ipo.is_multicast:
                                continue
                        if ipo.is_private:
                                continue
                        if ipo.is_unspecified:
                                continue
                        if ipo.is_reserved:
                                continue
                        if ipo.is_loopback:
                                continue
                        if ipo.is_link_local:
                                continue
                        if ipo.is_site_local:
                                continue

                        if ord(ipo.packed[11]) == 0xff and ord(ipo.packed[12]) == 0xfe:
                                return ip

                        if not noneui64:
                                noneui64 = ip

                return noneui64


def get_hostaddr():
        ipv4list = []
        ipv6list = []

        for i in interfaces:
                r = get_ipaddr(i)
                d(str(i)+": "+str(r))
                ipv4list += r[0]
                ipv6list += r[1]

        ipv4 = select_ipv4(ipv4list)
        ipv6 = select_ipv6(ipv6list)
                
        return (ipv4 if enable_ipv4 else None, ipv6 if enable_ipv6 else None)



def report_data_rest(url,data,name=None,password=None):
        h = httplib2.Http(disable_ssl_certificate_validation=True)
        if name and password:
                h.add_credentials(name, password)
        resp, content = h.request(url, "PUT",
                                  body=json.JSONEncoder().encode(data),
                                  headers={'content-type':'application/json'} )

        if int(resp['status']) == 200:
                d("Reported "+str(data)+" OK.")
		d("Result: "+content)
        else:
                log("Reporting "+str(data)+" failed with code "+str(resp['status'])+".")
		d("Result: "+content)


def main():
	global debug

	def usage():
		print """reportip.py by Tomas Hlavacek (tmshlvck@gmail.com)
  -d --debug : sets debugging output
  -h --help : prints this help message
"""

	try:
		opts, args = getopt.getopt(sys.argv[1:], "hd", ["help", "debug"])
	except getopt.GetoptError as err:
		print str(err)
		usage()
		sys.exit(2)

	for o, a in opts:
	        if o == '-d':
			debug = 1
		elif o == '-h':
			usage()
			sys.exit(0)
		else:
			assert False, "Unhandled option"


        if sleep:
		d("Sleeping for "+str(sleep)+"s.")
                time.sleep(sleep)

        # report addresses
        (ipv4,ipv6) = get_hostaddr()
        data = {'ipv4':ipv4, 'ipv6':ipv6}
        d("Sending: "+str(data))
        report_data_rest(report_url, data, http_name, http_password)
                

if __name__ == '__main__':
        main()
