"""A proxy that demands NTLM, for testing a client's 407 handshake (mitmproxy addon).

Squid's ntlm_fake_auth sends a malformed Type 2 (TargetName offset 3, no target info) that
Windows SSPI rejects with SEC_E_INVALID_TOKEN. This addon sends a well-formed NTLMv2 challenge
and accepts any Type 3: it tests the protocol (three legs on one connection), not the identity.
Run with --ignore-hosts '.*' so tunnels are not intercepted. Logs one line per leg.
"""
import base64
import os
import struct
import time

from mitmproxy import ctx, http

AUTHED = set()  # client connection ids that finished the handshake


def _av(av_id: int, value: bytes) -> bytes:
    return struct.pack("<HH", av_id, len(value)) + value


def challenge() -> bytes:
    u = lambda s: s.encode("utf-16-le")  # noqa: E731
    filetime = int((time.time() + 11644473600) * 10_000_000)
    info = (
        _av(2, u("LAB")) + _av(1, u("PROXY")) + _av(4, u("lab.test")) + _av(3, u("proxy.lab.test"))
        + _av(7, struct.pack("<Q", filetime)) + _av(0, b"")
    )
    target = u("LAB")
    # UNICODE | REQUEST_TARGET | NTLM | ALWAYS_SIGN | TARGET_TYPE_DOMAIN | EXTENDED_SESSIONSECURITY
    # | TARGET_INFO | VERSION | 128 | 56
    flags = 0x00000001 | 0x00000004 | 0x00000200 | 0x00008000 | 0x00010000 | 0x00080000 \
        | 0x00800000 | 0x02000000 | 0x20000000 | 0x80000000
    header_len = 56
    msg = b"NTLMSSP\x00" + struct.pack("<I", 2)
    msg += struct.pack("<HHI", len(target), len(target), header_len)
    msg += struct.pack("<I", flags) + os.urandom(8) + b"\x00" * 8
    msg += struct.pack("<HHI", len(info), len(info), header_len + len(target))
    msg += bytes([10, 0]) + struct.pack("<H", 19041) + b"\x00\x00\x00\x0f"  # version 10.0.19041, rev 15
    return msg + target + info


def type3_user(raw: bytes) -> str:
    def field(off):
        ln, _, o = struct.unpack("<HHI", raw[off:off + 8])
        return raw[o:o + ln].decode("utf-16-le", "replace")
    try:
        return f"{field(28)}\\{field(36)}"
    except Exception:  # noqa: BLE001
        return "?"


class NtlmProxyAuth:
    def http_connect(self, flow: http.HTTPFlow) -> None:
        cid = flow.client_conn.id
        if cid in AUTHED:
            return
        hdr = flow.request.headers.get("Proxy-Authorization", "")
        if hdr.startswith("NTLM "):
            raw = base64.b64decode(hdr[5:])
            kind = struct.unpack("<I", raw[8:12])[0] if len(raw) >= 12 else 0
            if kind == 1:
                c = base64.b64encode(challenge()).decode()
                ctx.log.info(f"NTLM leg1 {cid[:8]} -> challenge {c}")
                flow.response = http.Response.make(
                    407, b"", {"Proxy-Authenticate": "NTLM " + c,
                               "Content-Length": "0", "Connection": "keep-alive"})
                return
            if kind == 3:
                AUTHED.add(cid)
                ctx.log.info(f"NTLM leg3 {cid[:8]} accepted user={type3_user(raw)} ntlmv2={len(raw) > 200}")
                return
        ctx.log.info(f"NTLM 407 {cid[:8]} {flow.request.host}")
        flow.response = http.Response.make(
            407, b"", {"Proxy-Authenticate": "NTLM", "Content-Length": "0", "Connection": "keep-alive"})

    def requestheaders(self, flow: http.HTTPFlow) -> None:
        # Plain HTTP through this proxy is out of scope: answer it like a CONNECT-only proxy would.
        if flow.request.method != "CONNECT" and flow.client_conn.id not in AUTHED:
            flow.response = http.Response.make(407, b"", {"Proxy-Authenticate": "NTLM", "Content-Length": "0"})


addons = [NtlmProxyAuth()]
