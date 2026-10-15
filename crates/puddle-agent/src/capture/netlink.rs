// SPDX-License-Identifier: GPL-3.0-or-later
//! Just enough netlink to program the guest kernel: message framing, attributes, and (on Linux)
//! a blocking request/acknowledge exchange.
//!
//! The image has no firewall tool and no `ip` binary, and the agent builds the same on a
//! Windows host without a C toolchain, so the rules and interfaces are written to the kernel
//! directly: the `nf_tables` crates need `libclang` and the kernel headers at build time (`bindgen`),
//! a C library (`libnftnl`) or the `nft` binary. [`Writer`] builds the messages; [`nft`](super::nft) and [`link`](super::link) say
//! what goes in them.

/// `NLM_F_REQUEST`.
pub(crate) const F_REQUEST: u16 = 0x1;
/// `NLM_F_ACK`: answer with an error message whose code is 0 on success.
pub(crate) const F_ACK: u16 = 0x4;
/// `NLM_F_DUMP` (`NLM_F_ROOT | NLM_F_MATCH`).
#[cfg(any(target_os = "linux", test))]
pub(crate) const F_DUMP: u16 = 0x300;
/// `NLM_F_REPLACE`.
pub(crate) const F_REPLACE: u16 = 0x100;
/// `NLM_F_EXCL`.
pub(crate) const F_EXCL: u16 = 0x200;
/// `NLM_F_CREATE`.
pub(crate) const F_CREATE: u16 = 0x400;
/// `NLM_F_APPEND`.
pub(crate) const F_APPEND: u16 = 0x800;

/// `NLA_F_NESTED`.
const NESTED: u16 = 0x8000;
/// Length of the netlink header.
pub(crate) const HEADER: usize = 16;
/// `NLMSG_ERROR` (also the acknowledgement).
const ERROR: u16 = 2;
/// `NLMSG_DONE`: the end of a dump.
#[cfg(any(target_os = "linux", test))]
const DONE: u16 = 3;

/// Builds netlink messages into one buffer.
#[derive(Debug, Default)]
pub(crate) struct Writer {
    buf: Vec<u8>,
}

/// Where a message started, to patch its length when it ends.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Start(usize);

impl Writer {
    /// Starts a message with its 16-byte header (length patched by [`Writer::end`]).
    pub(crate) fn begin(&mut self, ty: u16, flags: u16, seq: u32) -> Start {
        let start = self.buf.len();
        self.buf.extend_from_slice(&[0; 4]);
        self.buf.extend_from_slice(&ty.to_ne_bytes());
        self.buf.extend_from_slice(&flags.to_ne_bytes());
        self.buf.extend_from_slice(&seq.to_ne_bytes());
        self.buf.extend_from_slice(&0u32.to_ne_bytes());
        Start(start)
    }

    /// Ends the message begun at `start`.
    pub(crate) fn end(&mut self, start: Start) {
        self.align();
        let len = u32::try_from(self.buf.len().saturating_sub(start.0)).unwrap_or(u32::MAX);
        if let Some(slot) = self.buf.get_mut(start.0..start.0 + 4) {
            slot.copy_from_slice(&len.to_ne_bytes());
        }
    }

    /// The `nfgenmsg` that starts an nfnetlink message body: family, version 0, and the
    /// resource id (big endian).
    pub(crate) fn nfgen(&mut self, family: u8, res_id: u16) {
        self.buf.push(family);
        self.buf.push(0);
        self.buf.extend_from_slice(&res_id.to_be_bytes());
    }

    /// Fixed bytes of a message body (a struct such as `ifinfomsg`).
    pub(crate) fn raw(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// One attribute.
    pub(crate) fn attr(&mut self, ty: u16, data: &[u8]) {
        self.header(ty, data.len());
        self.buf.extend_from_slice(data);
        self.align();
    }

    /// A NUL-terminated string attribute.
    pub(crate) fn string(&mut self, ty: u16, text: &str) {
        let mut data = text.as_bytes().to_vec();
        data.push(0);
        self.attr(ty, &data);
    }

    /// A big-endian `u32` attribute (nfnetlink's byte order).
    pub(crate) fn be32(&mut self, ty: u16, value: u32) {
        self.attr(ty, &value.to_be_bytes());
    }

    /// A nested attribute whose content `fill` writes.
    pub(crate) fn nested(&mut self, ty: u16, fill: impl FnOnce(&mut Self)) {
        let at = self.buf.len();
        self.header(ty | NESTED, 0);
        fill(self);
        let len = u16::try_from(self.buf.len().saturating_sub(at)).unwrap_or(u16::MAX);
        if let Some(slot) = self.buf.get_mut(at..at + 2) {
            slot.copy_from_slice(&len.to_ne_bytes());
        }
    }

    /// The bytes written so far.
    pub(crate) fn finish(self) -> Vec<u8> {
        self.buf
    }

    fn header(&mut self, ty: u16, payload: usize) {
        let len = u16::try_from(payload.saturating_add(4)).unwrap_or(u16::MAX);
        self.buf.extend_from_slice(&len.to_ne_bytes());
        self.buf.extend_from_slice(&ty.to_ne_bytes());
    }

    fn align(&mut self) {
        while !self.buf.len().is_multiple_of(4) {
            self.buf.push(0);
        }
    }
}

/// One message read from the kernel: its type and everything after the 16-byte header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Message<'a> {
    pub(crate) ty: u16,
    pub(crate) seq: u32,
    pub(crate) body: &'a [u8],
}

/// The native-endian `u32` at `at`.
fn word(buf: &[u8], at: usize) -> Option<u32> {
    buf.get(at..at + 4)
        .and_then(|b| b.try_into().ok())
        .map(u32::from_ne_bytes)
}

/// The native-endian `u16` at `at`.
fn half(buf: &[u8], at: usize) -> Option<u16> {
    buf.get(at..at + 2)
        .and_then(|b| b.try_into().ok())
        .map(u16::from_ne_bytes)
}

/// Splits a receive buffer into its messages. A truncated message ends the list.
pub(crate) fn messages(mut buf: &[u8]) -> Vec<Message<'_>> {
    let mut out = Vec::new();
    while let Some(len) = word(buf, 0).map(|len| len as usize) {
        let Some(msg) = buf.get(..len).filter(|_| len >= HEADER) else {
            break;
        };
        out.push(Message {
            ty: half(msg, 4).unwrap_or(0),
            seq: word(msg, 8).unwrap_or(0),
            body: msg.get(HEADER..).unwrap_or_default(),
        });
        buf = buf.get(len.div_ceil(4) * 4..).unwrap_or_default();
    }
    out
}

/// The attributes in `buf` (after any fixed struct), as `(type without flags, payload)`.
pub(crate) fn attrs(mut buf: &[u8]) -> Vec<(u16, &[u8])> {
    let mut out = Vec::new();
    while let (Some(len), Some(ty)) = (half(buf, 0), half(buf, 2)) {
        let len = usize::from(len).max(4);
        let Some(payload) = buf.get(4..len) else {
            break;
        };
        out.push((ty & !NESTED & !0x4000, payload));
        buf = buf.get(len.div_ceil(4) * 4..).unwrap_or_default();
    }
    out
}

/// What the kernel answered with an error message: `Ok` for an acknowledgement.
pub(crate) fn ack(message: &Message<'_>) -> Option<Result<(), (i32, u32)>> {
    if message.ty != ERROR {
        return None;
    }
    let code = message
        .body
        .get(..4)
        .and_then(|b| b.try_into().ok())
        .map_or(-EPROTO, i32::from_ne_bytes);
    Some(if code == 0 {
        Ok(())
    } else {
        Err((-code, message.seq))
    })
}

/// `EPROTO`, for an error message too short to hold a code.
const EPROTO: i32 = 71;

/// Whether `message` is the netfilter log's packet type.
pub(crate) fn is_packet(message: &Message<'_>) -> bool {
    message.ty == NFLOG_PACKET
}

#[cfg(target_os = "linux")]
pub(crate) use sys::{Socket, subscribe_nflog};

#[cfg(target_os = "linux")]
mod sys {
    use std::io;
    use std::os::fd::OwnedFd;
    use std::time::Duration;

    use rustix::net::netlink::SocketAddrNetlink;
    use rustix::net::sockopt::{Timeout, set_socket_timeout};
    use rustix::net::{
        AddressFamily, RecvFlags, SendFlags, SocketType, bind, netlink, recv, send, socket,
    };

    use super::{DONE, ack, messages};

    /// A netlink socket of one protocol with a receive timeout.
    #[derive(Debug)]
    pub(crate) struct Socket {
        fd: OwnedFd,
    }

    /// An error the kernel answered with: its OS error number, so callers can tell `ENOENT`
    /// from `EPERM`.
    fn kernel(errno: i32) -> io::Error {
        io::Error::from_raw_os_error(errno)
    }

    impl Socket {
        /// Opens a netlink socket of `protocol` (`netlink::ROUTE`, `netlink::NETFILTER`).
        pub(crate) fn open(
            protocol: Option<rustix::net::Protocol>,
            timeout: Duration,
        ) -> io::Result<Self> {
            let fd = socket(AddressFamily::NETLINK, SocketType::RAW, protocol)?;
            bind(&fd, &SocketAddrNetlink::new(0, 0))?;
            set_socket_timeout(&fd, Timeout::Recv, Some(timeout))?;
            Ok(Self { fd })
        }

        /// The route netlink socket.
        pub(crate) fn route() -> io::Result<Self> {
            Self::open(None, Duration::from_secs(5))
        }

        /// The netfilter netlink socket.
        pub(crate) fn netfilter() -> io::Result<Self> {
            Self::open(Some(netlink::NETFILTER), Duration::from_secs(5))
        }

        /// Sends `request` and waits until `acks` messages were acknowledged. The first kernel
        /// error is the result.
        pub(crate) fn transact(&self, request: &[u8], acks: usize) -> io::Result<()> {
            self.send_all(request)?;
            let mut seen = 0;
            let mut failure = None;
            let mut buf = vec![0u8; 64 * 1024];
            while seen < acks {
                let (len, _) = recv(&self.fd, buf.as_mut_slice(), RecvFlags::empty())?;
                for message in messages(buf.get(..len).unwrap_or_default()) {
                    match ack(&message) {
                        Some(Ok(())) => seen += 1,
                        Some(Err((code, seq))) => {
                            seen += 1;
                            failure.get_or_insert((code, seq));
                        }
                        None => {}
                    }
                }
            }
            match failure {
                None => Ok(()),
                Some((code, _)) => Err(kernel(code)),
            }
        }

        /// Sends a dump `request` and returns the body of every reply message of type
        /// `want`, up to the end of the dump.
        pub(crate) fn dump(&self, request: &[u8], want: u16) -> io::Result<Vec<Vec<u8>>> {
            self.send_all(request)?;
            let mut out = Vec::new();
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let (len, _) = recv(&self.fd, buf.as_mut_slice(), RecvFlags::empty())?;
                for message in messages(buf.get(..len).unwrap_or_default()) {
                    if message.ty == DONE {
                        return Ok(out);
                    }
                    if let Some(Err((code, _))) = ack(&message) {
                        return Err(kernel(code));
                    }
                    if message.ty == want {
                        out.push(message.body.to_vec());
                    }
                }
            }
        }

        /// The index of interface `name`.
        pub(crate) fn interface_index(&self, name: &str) -> io::Result<u32> {
            rustix::net::netdevice::name_to_index(&self.fd, name).map_err(io::Error::from)
        }

        /// Reads one datagram (a receive timeout is an error of kind `WouldBlock`/`TimedOut`).
        pub(crate) fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
            let (_, len) = recv(&self.fd, buf, RecvFlags::empty())?;
            Ok(len)
        }

        fn send_all(&self, request: &[u8]) -> io::Result<()> {
            let sent = send(&self.fd, request, SendFlags::empty())?;
            if sent == request.len() {
                Ok(())
            } else {
                Err(io::Error::other("short netlink write"))
            }
        }
    }

    /// Opens a netfilter socket subscribed to log group `group`, copying up to `range` bytes of each
    /// packet. Nothing is delivered to it until a rule logs to that group.
    pub(crate) fn subscribe_nflog(group: u16, range: u32) -> io::Result<Socket> {
        let socket = Socket::open(Some(netlink::NETFILTER), Duration::from_millis(500))?;
        let mut w = super::Writer::default();
        let bind = w.begin(super::NFLOG_CONFIG, super::F_REQUEST | super::F_ACK, 1);
        w.nfgen(0, group);
        w.attr(super::NFULA_CFG_CMD, &[super::NFULNL_CFG_CMD_BIND, 0, 0, 0]);
        w.end(bind);
        let mode = w.begin(super::NFLOG_CONFIG, super::F_REQUEST | super::F_ACK, 2);
        w.nfgen(0, group);
        let mut config = range.to_be_bytes().to_vec();
        config.extend_from_slice(&[super::NFULNL_COPY_PACKET, 0]);
        w.attr(super::NFULA_CFG_MODE, &config);
        w.end(mode);
        socket.transact(&w.finish(), 2)?;
        Ok(socket)
    }
}

/// `(NFNL_SUBSYS_ULOG << 8) | NFULNL_MSG_PACKET`.
pub(crate) const NFLOG_PACKET: u16 = 4 << 8;
/// `(NFNL_SUBSYS_ULOG << 8) | NFULNL_MSG_CONFIG`.
#[cfg(any(target_os = "linux", test))]
pub(crate) const NFLOG_CONFIG: u16 = (4 << 8) | 1;
/// `NFULA_CFG_CMD`.
#[cfg(any(target_os = "linux", test))]
const NFULA_CFG_CMD: u16 = 1;
/// `NFULA_CFG_MODE`.
#[cfg(any(target_os = "linux", test))]
const NFULA_CFG_MODE: u16 = 2;
/// `NFULNL_CFG_CMD_BIND`.
#[cfg(any(target_os = "linux", test))]
const NFULNL_CFG_CMD_BIND: u8 = 1;
/// `NFULNL_COPY_PACKET`.
#[cfg(any(target_os = "linux", test))]
const NFULNL_COPY_PACKET: u8 = 2;
/// `NFULA_PAYLOAD`.
pub(crate) const NFULA_PAYLOAD: u16 = 9;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nested_attribute_covers_its_children_and_every_attribute_is_aligned() {
        let mut w = Writer::default();
        let start = w.begin(0x0a00, F_REQUEST, 7);
        w.nfgen(1, 0);
        w.nested(4, |w| {
            w.string(1, "ab");
            w.be32(2, 5);
        });
        w.end(start);
        let bytes = w.finish();
        assert_eq!(bytes.len() % 4, 0);
        let all = messages(&bytes);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].ty, 0x0a00);
        assert_eq!(all[0].seq, 7);
        let inner = attrs(&all[0].body[4..]);
        assert_eq!(inner.len(), 1);
        assert_eq!(inner[0].0, 4);
        let children = attrs(inner[0].1);
        assert_eq!(children[0], (1, &b"ab\0"[..]));
        assert_eq!(children[1], (2, &[0, 0, 0, 5][..]));
    }

    #[test]
    fn a_truncated_buffer_yields_only_whole_messages() {
        let mut w = Writer::default();
        let a = w.begin(1, 0, 1);
        w.end(a);
        let mut bytes = w.finish();
        let whole = bytes.len();
        bytes.extend_from_slice(&[64, 0, 0, 0, 1, 0]);
        assert_eq!(messages(&bytes).len(), 1);
        assert_eq!(messages(&bytes[..whole - 1]).len(), 0);
        assert_eq!(attrs(&[9, 0, 1, 0, 1]).len(), 0);
        assert_eq!(
            messages(&[0u8; 16]).len(),
            0,
            "a message shorter than its own header"
        );
    }

    #[test]
    fn acknowledgements_and_errors_are_told_apart() {
        let mut body = 0i32.to_ne_bytes().to_vec();
        body.extend_from_slice(&[0; 16]);
        let ok = Message {
            ty: ERROR,
            seq: 3,
            body: &body,
        };
        assert_eq!(ack(&ok), Some(Ok(())));
        let mut body = (-17i32).to_ne_bytes().to_vec();
        body.extend_from_slice(&[0; 16]);
        let bad = Message {
            ty: ERROR,
            seq: 4,
            body: &body,
        };
        assert_eq!(ack(&bad), Some(Err((17, 4))));
        let other = Message {
            ty: 9,
            seq: 0,
            body: &[],
        };
        assert_eq!(ack(&other), None);
        let short = Message {
            ty: ERROR,
            seq: 5,
            body: &[1],
        };
        assert_eq!(ack(&short), Some(Err((71, 5))));
    }
}
