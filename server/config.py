# DDNS
# (C) 2014, Tomas Hlavacek (tmshlvck@gmail.com)
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


datadir = '/var/lib/...'
bin_nsupdate = '/usr/bin/nsupdate'
bin_dig = '/usr/bin/dig'
nsupdate_key = 'hmac-md5:<keyname>:<secret>'

dns_server = '127.0.0.1'
dns_zone = 'zone.tld'
rr_ttl = 60

allowed_names = ['name',]

def get_logfile():
    return datadir+'/ddns.log'

