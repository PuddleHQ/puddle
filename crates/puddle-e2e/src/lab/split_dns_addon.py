"""Split DNS for the lab interceptor: names only this proxy resolves, all pointing at loopback.

`NAMES` is replaced with the list of names when the fixture writes the file.
"""
from mitmproxy.proxy import server_hooks

NAMES = {__NAMES__}


class SplitDns:
    def server_connect(self, data: server_hooks.ServerConnectionHookData) -> None:
        host, port = data.server.address
        if host in NAMES:
            data.server.address = ("127.0.0.1", port)


addons = [SplitDns()]
