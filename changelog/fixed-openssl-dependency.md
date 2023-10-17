Change from openSSL to rustls. 

The prebuilt binaries depended on the system openSSL installation on Linux.
This meant that they required openSSL1, which is not supported e.g. on Ubuntu 22.04.
Changing to rustls removes this dependency.
