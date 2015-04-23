#!/usr/bin/env python
#
# Inteligent Home
# (C) 2015, Tomas Hlavacek (tmshlvck@gmail.com)
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


from flask import Flask
from flask import request
from flask import jsonify
from flask import make_response
import time
import sys
import re
import subprocess

import config

ipv4regexp = re.compile('([0-9]{1,3}\.){3}[0-9]{1,3}')
ipv6regexp = re.compile('([0-9a-fA-F]{0,4}:){1,7}[0-9a-fA-F]{0,4}')

def log(text):
        with open(config.get_logfile(),'a') as l:
                l.write(time.strftime("%c") + ' ' + text + "\n")


def normalize_dns(name):
        if name[-1] == '.':
                return name
        else:
                return name+'.'

def denormalize_dns(name):
        if name[-1] == '.':
                return name[:-1]
        else:
                return name
                

def update_dns(name, data):
        replace=False
        ipv4=None
        ipv6=None

        if 'ipv4' in data and data['ipv4'] and ipv4regexp.match(data['ipv4']):
                ipv4=data['ipv4']
        if 'ipv6' in data and data['ipv6'] and ipv6regexp.match(data['ipv6']):
                ipv6=data['ipv6']

        commands = """server %s
zone %s
update del %s
""" % (config.dns_server, denormalize_dns(config.dns_zone), normalize_dns(name+'.'+config.dns_zone))

        if ipv6:
                commands += "update add %s 3600 AAAA %s\n" % (normalize_dns(name+'.'+config.dns_zone), ipv6)
        if ipv4:
                commands += "update add %s 3600 A %s\n" % (normalize_dns(name+'.'+config.dns_zone), ipv4)

        commands += "send\n"

        log("Running following command "+config.bin_nsupdate+" -y "+config.nsupdate_key)
        nsu=subprocess.Popen([config.bin_nsupdate, "-y", config.nsupdate_key],
                             stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        log("Feeding following data: \n"+commands+"---------------")
        r=nsu.communicate(commands)
        log("Update finished. Output: "+str(r))


def query_dns(name):
        def q(name,qtype):
                regexpc=None
                addrregexp=None
                if qtype == 'A':
                        regexp = '^%s\s+[0-9]+\s+IN\s+A\s+([0-9\.]+)$'%normalize_dns(name+'.'+config.dns_zone)
                        regexpc = re.compile(regexp)
                        addrregexp = ipv4regexp
                elif qtype == 'AAAA':
                        regexp = '^%s\s+[0-9]+\s+IN\s+AAAA\s+([0-9a-fA-F:]+)$'%normalize_dns(name+'.'+config.dns_zone)
                        regexpc = re.compile(regexp)
                        addrregexp = ipv6regexp
                else:
                        raise Exception("Unknown qtype "+str(qtype))
                
                c=[config.bin_dig, '@'+config.dns_server, normalize_dns(name+'.'+config.dns_zone), qtype]
                nsu=subprocess.Popen(c,stdout=subprocess.PIPE)
                log("Running command: "+str(c))
                r=nsu.communicate()
                log("Command finished. Output: "+str(r))

                if r and r[0]:
                        for l in r[0].split('\n'):
                                m = regexpc.match(l)
                                if m and addrregexp.match(m.group(1)):
                                        return m.group(1)
                        

        response={}
        response['ipv4']=q(name,'A')
        response['ipv6']=q(name,'AAAA')
        log("Parsed response: "+str(response))
        return response


app = Flask(__name__.split('.')[0])

@app.route("/update/<name>", methods=['PUT'])
def update(name):
        if request.method == 'PUT':
                try:
                        name=name.decode('utf8').encode('ascii')
                        if name in config.allowed_names:
                                data=request.get_json()
                                q=query_dns(name)
                                log("Compare: "+str(data)+" ?= "+str(q)+" "+str(q==data))
                                if q != data:
                                        update_dns(name, data)
                                else:
                                        log("Update data are still the same. Nothing to do.")
                                return make_response(jsonify({'status':'OK'}),200)

                except Exception as e:
			log(str(e))
                        sys.stderr.write(str(e))
                        return make_response(jsonify({'status':'FAIL', 'error':str(e)}),500)

        return make_response(jsonify({'status':'NOOP'}),200)

@app.route("/get/<name>", methods=['GET'])
def get(name):
        response={'ipv4':None, 'ipv6':None}
        if request.method == 'GET':
                try:
                        name=name.decode('utf8').encode('ascii')
                        if name in config.allowed_names:
                                response = query_dns(name)

                except Exception as e:
			log(str(e))
                        sys.stderr.write(str(e))
                        return make_response(jsonify({'status':'FAIL', 'error':str(e)}),500)

        return make_response(jsonify(response),200)


if __name__ == '__main__':
        app.debug = True
        app.run()  
