#!/usr/bin/env python

from setuptools import setup

setup(name='ddns',
    version='1.0',
    description='Dynamic DNS host side',
    author='Tomas Hlavacek',
    author_email='tmshlvck@gmail.com',
    url='https://github.com/tmshlvck/ddns',
    install_requires = [
        'pyyaml',
        ],
    scripts = [
        'ddns-reportip.py',
        ],
   )

