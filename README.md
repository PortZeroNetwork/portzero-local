# Port Zero

Developers have all seen port conflict errors like this one:

```
Error: listen EADDRINUSE: address already in use :::3000
```

Port Zero is a developer tool that solves this problem once and for all. Port Zero has two parts:

- Port Zero Local solves this problem for traffic on a single computer, and is free & open source
- Port Zero Cloud solves this problem for devices on the LAN and over the Internet, and requires a subscription

Either way, you use Port Zero with your programs the same, whether they are a process or a Docker container. Configure all ports to 0 for any program you want Port Zero to manage; this tells the operating system to pick an available port at random. Then you start your programs with the `PZ_TUNNEL` environment variable. For example:

- If you specify `PZ_TUNNEL={branch}.mytodoapp.portzero.local:80`, that is a Local tunnel
- If you specify `PZ_TUNNEL={branch}.mytodoapp.<username>.portzero.cloud:80`, that is a Cloud tunnel

The `PZ_TUNNEL` setting tells PortZero the domain name and port that clients should use.

With your program running, you can open http://master.mytodoapp.portzero.local:80 in your browser. You might also be running a different version of your program in a separate git worktree. Port Zero supports this; http://some-other-branch.mytodoapp.portzero.local:80 can be available at the same time without port conflicts. This doesn't just work for http; it works for *any* TCP protocol.

## How does this work?

Port Zero runs a background process on your local dev machine that scans for processes and Docker containers with the special `PZ_TUNNEL` environment variable. If `PZ_TUNNEL` ends in `<username>.portzero.cloud`, Port Zero opens a Cloud tunnel to `portzero.cloud` under that username-scoped subtree. If on the other hand the `PZ_TUNNEL` contains `portzero.local`, Port Zero opens a Local tunnel and does four things:

1. Create a virtual network interface card (NIC) on your local machine if Port Zero has not already done so
2. Create a virtual IP address in this virtual NIC for that process
3. Create a virtual DNS record for that virtual IP address, based on the template specified in `PZ_TUNNEL`
4. Forward the port specified in `PZ_TUNNEL` on the virtual IP address to the randomly-assigned port on the actual process or Docker container
