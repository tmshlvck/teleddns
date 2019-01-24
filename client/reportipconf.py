# Put this file to /etc/ddns/
# Change the values to proper paths/ifaces/URLs
#
# Config for DDNS reportip.py

hostname="testhost"
enable_ipv6=True
enable_ipv4=False
interfaces=['eth0','wlan0']
bin_ip='/bin/ip'
sleep = 0 # add some time when using the script with NetworkManager
debug = 1

bin_nsupdate = '/usr/bin/nsupdate'
bin_dig = '/usr/bin/dig'
nsupdate_key = 'hmac-md5:<keyname>:<secret>'

dns_server = 'mydnsserver.org'
dns_zone = 'zone.tld'
rr_ttl = 60

