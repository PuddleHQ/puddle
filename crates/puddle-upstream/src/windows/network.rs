// SPDX-License-Identifier: GPL-3.0-or-later
//! The Windows [`NetworkOs`]: `GetAdaptersAddresses`, the IPv4 forwarding table and the registry
//! values that name DNS namespaces. Read-only; every raw resource is owned by a guard that
//! releases it.

use std::ffi::c_void;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use windows_sys::Win32::Foundation::{
    ERROR_BUFFER_OVERFLOW, ERROR_MORE_DATA, ERROR_NO_DATA, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS,
};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    FreeMibTable, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
    GetAdaptersAddresses, GetIpForwardTable2, IP_ADAPTER_ADDRESSES_LH, MIB_IPFORWARD_TABLE2,
};
use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows_sys::Win32::Networking::WinSock::{
    AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR, SOCKADDR_IN, SOCKADDR_IN6,
};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_ENUMERATE_SUB_KEYS, KEY_READ, RRF_RT_REG_MULTI_SZ, RRF_RT_REG_SZ,
    RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW,
};

use super::{read_wide, wide};
use crate::network::{Adapter, AdapterKind, NetworkError, NetworkOs, NetworkSnapshot, Route};

const TCPIP_PARAMETERS: &str = r"SYSTEM\CurrentControlSet\Services\Tcpip\Parameters";
const DNS_POLICY: &str = r"SOFTWARE\Policies\Microsoft\Windows NT\DNSClient";
/// Where the name-resolution policy table lives: group policy, and the local store that Always On
/// VPN and `Add-DnsClientNrptRule` write.
const NRPT_KEYS: [&str; 2] = [
    r"SOFTWARE\Policies\Microsoft\Windows NT\DNSClient\DnsPolicyConfig",
    r"SYSTEM\CurrentControlSet\Services\Dnscache\Parameters\DnsPolicyConfig",
];
/// Rows of the NRPT a read looks at at most; a machine has a handful.
const MAX_NRPT_RULES: u32 = 4096;

/// The Windows network, read on every call.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct WinNetwork;

impl NetworkOs for WinNetwork {
    fn read(&self) -> Result<NetworkSnapshot, NetworkError> {
        let adapters = adapters().map_err(NetworkError)?;
        let routes = routes().map_err(NetworkError)?;
        Ok(NetworkSnapshot {
            adapters,
            routes,
            dns_namespaces: dns_namespaces(),
        })
    }
}

/// Every adapter with its addresses, from `GetAdaptersAddresses`.
fn adapters() -> Result<Vec<Adapter>, String> {
    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let mut size: u32 = 16 * 1024;
    // The buffer is a `Vec<u64>` so it is aligned for the structures Windows writes into it.
    let mut buffer: Vec<u64> = Vec::new();
    for _ in 0..4 {
        buffer.resize((size as usize).div_ceil(8), 0);
        // SAFETY: `buffer` holds at least `size` writable bytes, and `size` is passed by pointer
        // so Windows can ask for more.
        let status = unsafe {
            GetAdaptersAddresses(
                u32::from(AF_UNSPEC),
                flags,
                std::ptr::null(),
                buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>(),
                &raw mut size,
            )
        };
        match status {
            ERROR_SUCCESS => {
                // SAFETY: Windows filled `buffer` with a linked list of adapters that stays valid
                // while `buffer` lives, and nothing writes to it meanwhile.
                return Ok(unsafe {
                    collect_adapters(buffer.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>())
                });
            }
            ERROR_NO_DATA => return Ok(Vec::new()),
            ERROR_BUFFER_OVERFLOW => {}
            other => return Err(format!("GetAdaptersAddresses: error {other}")),
        }
    }
    Err("GetAdaptersAddresses: the adapter list kept growing".to_owned())
}

/// Copies the adapter list `first` starts into owned values.
///
/// # Safety
/// `first` is null or the head of a list `GetAdaptersAddresses` wrote, valid for the call.
unsafe fn collect_adapters(first: *const IP_ADAPTER_ADDRESSES_LH) -> Vec<Adapter> {
    let mut out = Vec::new();
    let mut at = first;
    while !at.is_null() {
        // SAFETY: `at` is a list entry, per the caller's promise.
        let a = unsafe { &*at };
        let mut subnets = Vec::new();
        let mut unicast = a.FirstUnicastAddress;
        while !unicast.is_null() {
            // SAFETY: the unicast list belongs to the adapter entry and is valid with it.
            let entry = unsafe { &*unicast };
            // SAFETY: the socket address points into the same buffer, or is null.
            if let Some(ip) = unsafe { socket_ip(entry.Address.lpSockaddr) } {
                subnets.push((ip, entry.OnLinkPrefixLength));
            }
            unicast = entry.Next;
        }
        out.push(Adapter {
            // SAFETY: the union is written as `Length` and `IfIndex` by Windows.
            index: unsafe { a.Anonymous1.Anonymous.IfIndex },
            // SAFETY: the strings are null or terminated and live in the same buffer.
            name: unsafe { read_wide(a.FriendlyName) }.unwrap_or_default(),
            // SAFETY: as above.
            description: unsafe { read_wide(a.Description) }.unwrap_or_default(),
            kind: AdapterKind::from_if_type(a.IfType),
            up: a.OperStatus == IfOperStatusUp,
            // SAFETY: as above.
            dns_suffix: unsafe { read_wide(a.DnsSuffix) },
            subnets,
            metric: a.Ipv4Metric,
        });
        at = a.Next;
    }
    out
}

/// The address a socket address holds, `None` for null or another family.
///
/// # Safety
/// `sockaddr` is null or points at a socket address of the length its family says.
unsafe fn socket_ip(sockaddr: *const SOCKADDR) -> Option<IpAddr> {
    if sockaddr.is_null() {
        return None;
    }
    // SAFETY: every socket address starts with its family.
    match unsafe { (*sockaddr).sa_family } {
        AF_INET => {
            // SAFETY: an `AF_INET` address is a `SOCKADDR_IN`.
            let v4 = unsafe { sockaddr.cast::<SOCKADDR_IN>().read_unaligned() };
            // SAFETY: `S_addr` is the 4 address bytes in network order, whichever way the union is read.
            let raw = unsafe { v4.sin_addr.S_un.S_addr };
            Some(IpAddr::V4(Ipv4Addr::from(raw.to_ne_bytes())))
        }
        AF_INET6 => {
            // SAFETY: an `AF_INET6` address is a `SOCKADDR_IN6`.
            let v6 = unsafe { sockaddr.cast::<SOCKADDR_IN6>().read_unaligned() };
            // SAFETY: the 16 bytes are the address, whichever way the union is read.
            Some(IpAddr::V6(Ipv6Addr::from(unsafe { v6.sin6_addr.u.Byte })))
        }
        _ => None,
    }
}

/// Owns a table `GetIpForwardTable2` allocated and frees it.
struct ForwardTable(*mut MIB_IPFORWARD_TABLE2);

impl Drop for ForwardTable {
    fn drop(&mut self) {
        // SAFETY: the table came from `GetIpForwardTable2` and is not used after this.
        unsafe { FreeMibTable(self.0.cast::<c_void>()) };
    }
}

/// The IPv4 forwarding table.
fn routes() -> Result<Vec<Route>, String> {
    let mut table: *mut MIB_IPFORWARD_TABLE2 = std::ptr::null_mut();
    // SAFETY: `table` is a valid out-pointer; Windows allocates the table.
    let status = unsafe { GetIpForwardTable2(AF_INET, &raw mut table) };
    if status != ERROR_SUCCESS || table.is_null() {
        return Err(format!("GetIpForwardTable2: error {status}"));
    }
    let table = ForwardTable(table);
    // SAFETY: the table holds `NumEntries` rows from `Table` on, valid until `table` drops.
    let rows = unsafe {
        let head = &*table.0;
        std::slice::from_raw_parts(head.Table.as_ptr(), head.NumEntries as usize)
    };
    Ok(rows
        .iter()
        .map(|row| {
            // SAFETY: the table was asked for `AF_INET`, so every prefix is an IPv4 address.
            let raw = unsafe { row.DestinationPrefix.Prefix.Ipv4.sin_addr.S_un.S_addr };
            Route {
                net: IpAddr::V4(Ipv4Addr::from(raw.to_ne_bytes())),
                bits: row.DestinationPrefix.PrefixLength,
                interface: row.InterfaceIndex,
                metric: row.Metric,
            }
        })
        .collect())
}

/// The DNS namespaces the machine holds outside its adapters: primary suffix, suffix search lists
/// and the name-resolution policy table. What cannot be read is missing, not an error.
fn dns_namespaces() -> Vec<String> {
    let mut out = Vec::new();
    for key in [TCPIP_PARAMETERS, DNS_POLICY] {
        for value in ["Domain", "NV Domain", "PrimaryDnsSuffix"] {
            out.extend(registry_strings(HKEY_LOCAL_MACHINE, key, value));
        }
        for list in registry_strings(HKEY_LOCAL_MACHINE, key, "SearchList") {
            out.extend(list.split(',').map(str::to_owned));
        }
    }
    for key in NRPT_KEYS {
        for rule in subkeys(key) {
            out.extend(registry_strings(
                HKEY_LOCAL_MACHINE,
                &format!(r"{key}\{rule}"),
                "Name",
            ));
        }
    }
    out
}

/// A string or multi-string registry value, one entry per string; empty when it is not there.
fn registry_strings(root: HKEY, key: &str, value: &str) -> Vec<String> {
    let (key, value) = (wide(key), wide(value));
    let flags = RRF_RT_REG_SZ | RRF_RT_REG_MULTI_SZ;
    let mut size = 0u32;
    // SAFETY: both names are NUL-terminated; a null buffer asks for the size.
    let status = unsafe {
        RegGetValueW(
            root,
            key.as_ptr(),
            value.as_ptr(),
            flags,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut size,
        )
    };
    if status != ERROR_SUCCESS || size == 0 {
        return Vec::new();
    }
    let mut data = vec![0u16; (size as usize).div_ceil(2)];
    let mut got = size;
    // SAFETY: `data` holds `got` writable bytes; the names are NUL-terminated.
    let status = unsafe {
        RegGetValueW(
            root,
            key.as_ptr(),
            value.as_ptr(),
            flags,
            std::ptr::null_mut(),
            data.as_mut_ptr().cast::<c_void>(),
            &raw mut got,
        )
    };
    if status != ERROR_SUCCESS && status != ERROR_MORE_DATA {
        return Vec::new();
    }
    let units = (got as usize / 2).min(data.len());
    data.get(..units)
        .unwrap_or_default()
        .split(|unit| *unit == 0)
        .filter(|part| !part.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

/// The names of the subkeys of `HKLM\<key>`; empty when the key is not there.
fn subkeys(key: &str) -> Vec<String> {
    let name = wide(key);
    let mut handle: HKEY = std::ptr::null_mut();
    // SAFETY: a NUL-terminated key name and a valid out-pointer.
    let opened = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            name.as_ptr(),
            0,
            KEY_READ | KEY_ENUMERATE_SUB_KEYS,
            &raw mut handle,
        )
    };
    if opened != ERROR_SUCCESS {
        return Vec::new();
    }
    let mut out = Vec::new();
    for index in 0..MAX_NRPT_RULES {
        let mut buffer = [0u16; 256];
        let mut len = 256u32;
        // SAFETY: `handle` is open; `buffer` holds `len` writable units.
        let status = unsafe {
            RegEnumKeyExW(
                handle,
                index,
                buffer.as_mut_ptr(),
                &raw mut len,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if status == ERROR_NO_MORE_ITEMS || status != ERROR_SUCCESS {
            break;
        }
        out.push(String::from_utf16_lossy(
            buffer.get(..len as usize).unwrap_or_default(),
        ));
    }
    // SAFETY: `handle` was opened above and is not used again.
    unsafe { RegCloseKey(handle) };
    out
}

#[cfg(test)]
mod tests;
