# SPDX-License-Identifier: GPL-3.0-or-later
# Test helper of the ci/fetch-*-test.sh scripts: python3 -I ci/flaky-server.py <folder> <fails> <log>
# serves the folder on a free 127.0.0.1 port (printed on the first line), drops the connection for the
# first <fails> requests (the way a stream error looks to curl) and logs one line per request.
import functools, http.server, sys

root, fails, log = sys.argv[1], int(sys.argv[2]), sys.argv[3]
seen = 0


class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        global seen
        seen += 1
        with open(log, "a") as f:
            f.write("%d %s\n" % (seen, self.path))
        if seen <= fails:
            self.connection.close()
            return
        super().do_GET()

    def log_message(self, *args):
        pass


server = http.server.HTTPServer(("127.0.0.1", 0), functools.partial(Handler, directory=root))
print(server.server_address[1], flush=True)
server.serve_forever()
