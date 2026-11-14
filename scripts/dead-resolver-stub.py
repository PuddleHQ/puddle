# SPDX-License-Identifier: AGPL-3.0-or-later
"""A DNS server that never answers: it appends the name of every query to the file in argv[2].

scripts/dead-resolver.sh runs it as the only nameserver of a test run. A lookup then waits for the
resolver's timeout instead of failing at once, as a real outage does, and the log says which names
were asked.
"""
import os
import socket
import sys


def qname(packet: bytes) -> str:
    labels, at = [], 12
    while at < len(packet) and packet[at] != 0:
        size = packet[at]
        labels.append(packet[at + 1 : at + 1 + size].decode("ascii", "replace"))
        at += size + 1
    return ".".join(labels)


def main() -> None:
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind((sys.argv[1], 53))
    sock.settimeout(0.5)
    parent = os.getppid()
    print("ready", flush=True)
    with open(sys.argv[2], "a", encoding="utf-8") as log:
        # The command replaces the script that started this server, so it is the parent: when it
        # ends the parent changes and the server ends too.
        while os.getppid() == parent:
            try:
                packet, _ = sock.recvfrom(4096)
            except socket.timeout:
                continue
            log.write(qname(packet) + "\n")
            log.flush()


main()
