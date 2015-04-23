#!/usr/bin/python

import sys
sys.path.insert(0, '/var/lib/.../')
from rest import app as application

from wsgiref.handlers import CGIHandler

# hack for bug in werkzeug
import os
if os.environ.get("PATH_INFO", None) is None:
	os.environ["PATH_INFO"] = ""

CGIHandler().run(application)

