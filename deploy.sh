#!/bin/bash

add_cron () {
	# $1 = path and filename of the ddns_reportip
	crontab -l >/tmp/ct.txt
	if grep "$1" /tmp/ct.txt >/dev/null; then
		echo "crontab already contains the ddns integration. Skipping."
	else
		echo "installing new record into crontab..."
		echo "*/30 *  * * *   root    $1" >>/tmp/ct.txt
		crontab /tmp/ct.txt
	fi
	rm /tmp/ct.txt
}

install_common () {
	python3 setup.py install

	if [ ! -f /etc/ddns/ddns.yaml ]; then
		mkdir /etc/ddns
		cp ddns.yaml /etc/ddns/
	fi
}

add_nm () {
	# $1 = path and filename of the ddns_reportip
	BF="$1"
	if [ -d /etc/NetworkManager/dispatcher.d/ ]; then
		sed "s!DDNS_REPORTIP_PY!${BF}!" 99dnsupdate.template >99dnsupdate
		chmod a+x 99dnsupdate
		cp 99dnsupdate /etc/NetworkManager/dispatcher.d/
	fi
}

clean () {
	rm -rf 99dnsupdate build/ ddns.egg-info/ dist/
}

deploy_debian () {
	clean
	apt-get update
	apt-get -y install python3-yaml python3-setuptools dnsutils
	install_common

	BF=`which ddns-reportip.py`
	if [ -z "${BF}" ] || [ ! -f ${BF} ]; then
		echo "Installation failed. Aborting system integration."
		exit -1
	fi

	add_cron $BF
	add_nm $BF
	clean
}

deploy_arch () {
	clean
	pikaur -Suy --needed python-yaml python-setuptools bind-tools
	install_common

	BF=`which ddns-reportip.py`
	if [ -z "${BF}" ] || [ ! -f ${BF} ]; then
		echo "Installation failed. Aborting system integration."
		exit -1
	fi

	add_cron $BF
	add_nm $BF
	clean
}

if [ -f /etc/debian_version ]; then
	deploy_debian
	exit 0
fi

if [ -f /etc/arch-release ]; then
	deploy_arch
	exit 0
fi

