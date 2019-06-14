#!/bin/bash

add_cron () {
	# $1 = prefix for the installed binary
	crontab -l > /tmp/ct.txt
	echo "*/39 *  * * *   root    $1/ddns-reportip.py >>/var/log/dnsupdate.log" >> /tmp/ct.txt
	crontab /tmp/ct.txt
	rm /tmp/ct.txt
}

install_common () {
	python3 setup.py install

	if ! [ -f /etc/ddns/ddns.yaml ]; then
		mkdir /etc/ddns
		cp ddns.yaml /etc/ddns/
	fi

	if [ -d /etc/NetworkManager/dispatcher.d/ ]; then
		cp 99dnsupdate /etc/NetworkManager/dispatcher.d/
	fi
}

deploy_debian () {
	apt-get -y install python3-yaml python3-setuptools dnsutils
	install_common
	add_cron /usr/local/bin
}

deploy_arch () {
	pikaur -Suy python-yaml python-setuptools bind-tools
	install_common
	add_cron /usr/bin
}

if [ -f /etc/debian_version ]; then
	deploy_debian
	exit 0
fi

if [ -f /etc/arch-release ]; then
	deploy_arch
	exit 0
fi

