# Put this file to /etc/ddns/
# Change the values to proper paths/ifaces/URLs
#
# Config for DDNS reportip.py
report_url = "https://<host>/ddns/rest/update/<name>"
http_name = '<name>'
http_password = '<passwd>'
enable_ipv6=True
enable_ipv4=False
interfaces=['eth0','wlan0']
bin_ip='/bin/ip'
sleep = 0 # add some time when using the script with network manager
debug = 1

