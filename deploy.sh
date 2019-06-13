#!/bin/bash

add_cron () {
	crontab -l > /tmp/ct.txt
	echo "*/39 *  * * *   root    /usr/bin/ddns-reportip.py >>/var/log/dnsupdate.log" >> /tmp/ct.txt
	crontab /tmp/ct.txt
	rm /tmp/ct.txt
}

install_common () {
	python3 setup.py install

	if ! [ -f /etc/ddns/ddns.yaml ]; then
		mkdir /etc/ddns
		cp ddns.yaml /etc/ddns/
	fi

	cp 99dnsupdate /etc/NetworkManager/dispatcher.d/
}

deploy_debian () {
	apt-get -y install python3-yaml dnsutils
	install_common
	add_cron
}

deploy_arch () {
	pikaur -Suy python-yaml bind-tools
	install_common
	add_cron
}

if [ -f /etc/debian_version ]; then
	deploy_debian
	exit 0
fi

if [ -f /etc/arch-release ]; then
	deploy_arch
	exit 0
fi
