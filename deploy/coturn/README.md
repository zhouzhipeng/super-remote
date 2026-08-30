# coturn certificates

Place `fullchain.pem` and `privkey.pem` in `deploy/coturn/certs/`. The relay UDP range is
49160–49200 and must be open in the host firewall/security group, together with 3478/udp,
3478/tcp and 5349/tcp. The compose configuration uses time-limited REST credentials and
does not enable anonymous TURN access.
