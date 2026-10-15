#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Which TLS alert does each tool send when it refuses a certificate? Puddle decrypts a connection
# with a certificate from the workspace's own CA; a tool that does not trust that CA refuses it. The
# proxy can only say "the tool refused the certificate" when the tool sends a certificate alert first;
# a tool that closes the connection without one shows up as an ordinary end of the connection.
#
#   ci/radar/tls-alerts.sh [tool...]      tools: curl node python go java (default: all)
#
# An `openssl s_server` (TLS 1.2 only, so the client's alert is not encrypted and `-msg` can print it) serves
# a certificate signed by a CA no tool trusts, and `-msg` prints the
# client's alert. curl, node and python run on this machine; go and java run in containers (docker), so
# a missing runtime is skipped with a line saying so. Output: one line per tool,
# `<tool>: <alert, or "no alert seen by s_server", or "connected (it trusted the certificate)">`.
# "No alert seen" is not proof that none was sent (JSSE's alert did not show here although its message says it
# sent one): the VM probe in `vm_radar_*` asks the real proxy, which decrypts TLS 1.3 alerts itself.
set -u -o pipefail

work=$(mktemp -d)
trap 'kill $(jobs -p) 2>/dev/null; rm -rf "$work"' EXIT
port=${TLS_ALERTS_PORT:-18443}

openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj "/CN=Workspace CA" \
	-keyout "$work/ca.key" -out "$work/ca.pem" 2>/dev/null
openssl req -newkey rsa:2048 -nodes -subj "/CN=localhost" -keyout "$work/leaf.key" -out "$work/leaf.csr" 2>/dev/null
printf 'subjectAltName=DNS:localhost,IP:127.0.0.1\n' >"$work/ext"
openssl x509 -req -in "$work/leaf.csr" -CA "$work/ca.pem" -CAkey "$work/ca.key" -CAcreateserial \
	-days 2 -extfile "$work/ext" -out "$work/leaf.pem" 2>/dev/null

cat >"$work/probe.go" <<'GO'
package main

import (
	"fmt"
	"net/http"
	"os"
)

func main() {
	r, err := http.Get(os.Args[1])
	if err != nil {
		fmt.Println("error:", err)
		os.Exit(1)
	}
	fmt.Println("status:", r.Status)
}
GO
cat >"$work/Probe.java" <<'JAVA'
import java.net.URI;
import java.net.http.*;

public class Probe {
    public static void main(String[] args) throws Exception {
        try {
            var r = HttpClient.newHttpClient().send(
                HttpRequest.newBuilder(URI.create(args[0])).build(), HttpResponse.BodyHandlers.ofString());
            System.out.println("status: " + r.statusCode());
        } catch (Exception e) {
            System.out.println("error: " + e);
            System.exit(1);
        }
    }
}
JAVA
cat >"$work/probe.js" <<'JS'
require('https').get(process.argv[2], (r) => { console.log('status: ' + r.statusCode); r.resume(); })
  .on('error', (e) => { console.log('error: ' + e.message); process.exit(1); });
JS
cat >"$work/probe.py" <<'PY'
import sys, urllib.request
try:
    print("status:", urllib.request.urlopen(sys.argv[1], timeout=20).status)
except Exception as e:
    print("error:", e)
    sys.exit(1)
PY

url="https://localhost:$port/"
# The tool's command; `host` ones see the server at localhost, containers share the host network.
run_tool() {
	case $1 in
	curl) command -v curl >/dev/null && curl -sS --max-time 20 "$url" ;;
	node) command -v node >/dev/null && node "$work/probe.js" "$url" ;;
	python) command -v python3 >/dev/null && python3 "$work/probe.py" "$url" ;;
	go) docker run --rm --network host -v "$work:/w:ro" -w /tmp golang:1.24-alpine go run /w/probe.go "$url" ;;
	java) docker run --rm --network host -v "$work:/w:ro" -w /tmp eclipse-temurin:21-jdk java /w/Probe.java "$url" ;;
	*) echo "unknown tool $1"; return 2 ;;
	esac
}

tools=("$@")
[ ${#tools[@]} -gt 0 ] || tools=(curl node python go java)
for tool in "${tools[@]}"; do
	log=$work/server-$tool.log
	openssl s_server -tls1_2 -accept "$port" -cert "$work/leaf.pem" -key "$work/leaf.key" -msg -www -quiet >"$log" 2>&1 &
	server=$!
	sleep 0.5
	out=$(run_tool "$tool" 2>&1)
	status=$?
	sleep 0.5
	kill "$server" 2>/dev/null
	wait "$server" 2>/dev/null
	# `-msg` prints the client's messages as `<<< TLS 1.3 Alert [length 0002], fatal unknown_ca`.
	alert=$(grep -E '^<<<.*Alert' -A1 "$log" | grep -oE '(fatal|warning) [a-z_]+' | sort -u | tr '\n' ',' | sed 's/,$//')
	if [ "$status" -eq 127 ] || [ "$status" -eq 125 ]; then
		echo "$tool: skipped (not available: ${out:0:80})"
	elif [ -n "$alert" ]; then
		echo "$tool: $alert   | tool said: $(echo "$out" | head -1 | cut -c1-140)"
	elif [ "$status" -eq 0 ]; then
		echo "$tool: connected (it trusted the certificate)"
	else
		echo "$tool: no alert seen by s_server   | tool said: $(echo "$out" | head -1 | cut -c1-140)"
	fi
done
