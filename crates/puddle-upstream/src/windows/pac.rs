// SPDX-License-Identifier: GPL-3.0-or-later
//! PAC and WPAD evaluation by Windows' own engine (`WinHttpGetProxyForUrlEx`): out of process,
//! cancellable, and it returns the whole list including `DIRECT` and failover entries.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use std::ffi::c_void;

use windows_sys::Win32::Networking::WinHttp::{
    ERROR_WINHTTP_AUTODETECTION_FAILED, ERROR_WINHTTP_LOGIN_FAILURE,
    ERROR_WINHTTP_UNABLE_TO_DOWNLOAD_SCRIPT, WINHTTP_ACCESS_TYPE_NO_PROXY, WINHTTP_ASYNC_RESULT,
    WINHTTP_AUTO_DETECT_TYPE_DHCP, WINHTTP_AUTO_DETECT_TYPE_DNS_A, WINHTTP_AUTOPROXY_AUTO_DETECT,
    WINHTTP_AUTOPROXY_CONFIG_URL, WINHTTP_AUTOPROXY_OPTIONS,
    WINHTTP_CALLBACK_FLAG_GETPROXYFORURL_COMPLETE, WINHTTP_CALLBACK_FLAG_REQUEST_ERROR,
    WINHTTP_CALLBACK_STATUS_GETPROXYFORURL_COMPLETE, WINHTTP_CALLBACK_STATUS_HANDLE_CLOSING,
    WINHTTP_CALLBACK_STATUS_REQUEST_ERROR, WINHTTP_FLAG_ASYNC, WINHTTP_INTERNET_SCHEME_HTTP,
    WINHTTP_PROXY_RESULT, WinHttpCloseHandle, WinHttpCreateProxyResolver, WinHttpFreeProxyResult,
    WinHttpGetProxyForUrlEx, WinHttpGetProxyResult, WinHttpOpen, WinHttpSetStatusCallback,
};

use super::{read_wide, wide};
use crate::hop::{Hop, ProxyAddr};
use crate::os::{PacError, PacQuery};

const ERROR_IO_PENDING: u32 = 997;
/// `WINHTTP_CALLBACK_FLAG_HANDLES` is the two handle statuses; the closing one has the same value
/// as its flag.
const CALLBACK_FLAG_HANDLE_CLOSING: u32 = WINHTTP_CALLBACK_STATUS_HANDLE_CLOSING;
/// How long a cancelled request may take to let go of its buffers before they are leaked instead.
const CLOSE_WAIT: Duration = Duration::from_secs(10);
const ERROR_WINHTTP_TIMEOUT: u32 = 12002;
const ERROR_WINHTTP_OPERATION_CANCELLED: u32 = 12017;

/// The result slot the WinHTTP callback fills and the caller waits on.
#[derive(Default)]
struct Slot {
    outcome: Mutex<Option<Result<(), u32>>>,
    ready: Condvar,
    /// Set by the `HANDLE_CLOSING` status: WinHTTP uses nothing of the request after it.
    closed: Mutex<bool>,
    closed_signal: Condvar,
}

impl Slot {
    fn finish(&self, outcome: Result<(), u32>) {
        let mut guard = self.outcome.lock().unwrap_or_else(PoisonError::into_inner);
        *guard = Some(outcome);
        // Notified while the lock is held, so a waiter that sees the outcome cannot run ahead of this call.
        self.ready.notify_all();
    }

    fn close(&self) {
        *self.closed.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.closed_signal.notify_all();
    }

    /// Whether the handle's closing was reported within `timeout`.
    fn wait_closed(&self, timeout: Duration) -> bool {
        let guard = self.closed.lock().unwrap_or_else(PoisonError::into_inner);
        *self
            .closed_signal
            .wait_timeout_while(guard, timeout, |closed| !*closed)
            .unwrap_or_else(PoisonError::into_inner)
            .0
    }

    fn wait(&self, timeout: Duration) -> Option<Result<(), u32>> {
        let deadline = Instant::now() + timeout;
        let mut guard = self.outcome.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(outcome) = guard.take() {
                return Some(outcome);
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            guard = self
                .ready
                .wait_timeout(guard, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

/// The slots of the PAC resolutions in flight, by resolution id. The id travels through WinHTTP as
/// the request context and is never dereferenced, so a resolution allocates nothing that outlives
/// it. WinHTTP can call back after `WinHttpCloseHandle` returns, until its `HANDLE_CLOSING`
/// status, so a late call must be harmless: it finds no entry for its id and does nothing.
struct Pending(Mutex<Vec<(usize, Arc<Slot>)>>);

impl Pending {
    const fn new() -> Self {
        Self(Mutex::new(Vec::new()))
    }

    fn entries(&self) -> MutexGuard<'_, Vec<(usize, Arc<Slot>)>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Files `slot` under a new id and returns the id.
    fn add(&self, slot: &Arc<Slot>) -> usize {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        self.entries().push((id, Arc::clone(slot)));
        id
    }

    fn remove(&self, id: usize) {
        // Dropped after the lock is released, as the slot's drop is not ours to constrain.
        let removed = {
            let mut entries = self.entries();
            entries
                .iter()
                .position(|(entry, _)| *entry == id)
                .map(|at| entries.swap_remove(at))
        };
        drop(removed);
    }

    /// The slot filed under `id`, if its resolution is still in flight.
    fn find(&self, id: usize) -> Option<Arc<Slot>> {
        self.entries()
            .iter()
            .find(|(entry, _)| *entry == id)
            .map(|(_, slot)| Arc::clone(slot))
    }
}

static PENDING: Pending = Pending::new();
static NEXT_ID: AtomicUsize = AtomicUsize::new(1);

/// A resolution's entry in `PENDING`, removed when it goes out of scope.
struct Registration(usize);

impl Registration {
    fn new(slot: &Arc<Slot>) -> Self {
        Self(PENDING.add(slot))
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        PENDING.remove(self.0);
    }
}

/// WinHTTP's status callback for one PAC resolution: fills the request's `Slot` if it is still
/// waiting for the answer.
///
/// # Safety
///
/// `info` must be what WinHTTP documents for `status`. `context` is only a lookup key, never
/// dereferenced. Only WinHTTP calls it.
unsafe extern "system" fn callback(
    _handle: *mut c_void,
    context: usize,
    status: u32,
    info: *mut c_void,
    _len: u32,
) {
    let Some(slot) = PENDING.find(context) else {
        return;
    };
    match status {
        WINHTTP_CALLBACK_STATUS_GETPROXYFORURL_COMPLETE => slot.finish(Ok(())),
        WINHTTP_CALLBACK_STATUS_HANDLE_CLOSING => slot.close(),
        WINHTTP_CALLBACK_STATUS_REQUEST_ERROR if !info.is_null() => {
            // SAFETY: for this status `info` points at a WINHTTP_ASYNC_RESULT.
            slot.finish(Err(unsafe {
                (*info.cast::<WINHTTP_ASYNC_RESULT>()).dwError
            }));
        }
        _ => {}
    }
}

/// Closes a WinHTTP handle on drop.
struct Handle(*mut c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the handle came from WinHTTP and is closed once.
            unsafe { WinHttpCloseHandle(self.0) };
        }
    }
}

pub(super) fn resolve(query: &PacQuery) -> Result<Vec<Hop>, PacError> {
    match resolve_once(query, false) {
        Err(PacError::Failed(ref message))
            if message.contains(&format!("({ERROR_WINHTTP_LOGIN_FAILURE})")) =>
        {
            // A PAC behind authentication: retry once as the signed-in user.
            resolve_once(query, true)
        }
        other => other,
    }
}

fn failure(code: u32) -> PacError {
    let detail = format!("WinHTTP error ({code})");
    match code {
        ERROR_WINHTTP_AUTODETECTION_FAILED | ERROR_WINHTTP_UNABLE_TO_DOWNLOAD_SCRIPT => {
            PacError::Unavailable(detail)
        }
        ERROR_WINHTTP_TIMEOUT => PacError::Timeout,
        _ => PacError::Failed(detail),
    }
}

fn resolve_once(query: &PacQuery, auto_logon: bool) -> Result<Vec<Hop>, PacError> {
    let agent = wide("puddle");
    // SAFETY: valid terminated agent name; null proxy arguments are allowed for NO_PROXY.
    let session = Handle(unsafe {
        WinHttpOpen(
            agent.as_ptr(),
            WINHTTP_ACCESS_TYPE_NO_PROXY,
            std::ptr::null(),
            std::ptr::null(),
            WINHTTP_FLAG_ASYNC,
        )
    });
    if session.0.is_null() {
        return Err(PacError::Failed(format!(
            "WinHttpOpen: {}",
            std::io::Error::last_os_error()
        )));
    }
    let mut raw_resolver = std::ptr::null_mut();
    // SAFETY: `session` is a live session handle; `raw_resolver` receives a new handle.
    let code = unsafe { WinHttpCreateProxyResolver(session.0, &raw mut raw_resolver) };
    if code != 0 {
        return Err(failure(code));
    }
    let resolver = Handle(raw_resolver);

    let slot = Arc::new(Slot::default());
    // Dropped before `resolver` on the paths that return with it live: a late callback in between
    // finds no entry and does nothing.
    let registration = Registration::new(&slot);
    // SAFETY: `callback` matches WINHTTP_STATUS_CALLBACK; flags name only the three statuses it handles.
    unsafe {
        WinHttpSetStatusCallback(
            resolver.0,
            Some(callback),
            WINHTTP_CALLBACK_FLAG_REQUEST_ERROR
                | WINHTTP_CALLBACK_FLAG_GETPROXYFORURL_COMPLETE
                | CALLBACK_FLAG_HANDLE_CLOSING,
            0,
        );
    }

    let pac_url = query.pac_url.as_deref().map(wide);
    let mut flags = 0;
    if pac_url.is_some() {
        flags |= WINHTTP_AUTOPROXY_CONFIG_URL;
    }
    if query.auto_detect {
        flags |= WINHTTP_AUTOPROXY_AUTO_DETECT;
    }
    let options = WINHTTP_AUTOPROXY_OPTIONS {
        dwFlags: flags,
        dwAutoDetectFlags: if query.auto_detect {
            WINHTTP_AUTO_DETECT_TYPE_DHCP | WINHTTP_AUTO_DETECT_TYPE_DNS_A
        } else {
            0
        },
        lpszAutoConfigUrl: pac_url.as_ref().map_or(std::ptr::null(), Vec::as_ptr),
        lpvReserved: std::ptr::null_mut(),
        dwReserved: 0,
        fAutoLogonIfChallenged: i32::from(auto_logon),
    };
    let url = wide(&query.url);
    // SAFETY: the resolver is live. Microsoft's page for the call
    // (https://learn.microsoft.com/windows/win32/api/winhttp/nf-winhttp-winhttpgetproxyforurlex)
    // does not say WinHTTP copies `url` or `options`, so they are treated as in use until the
    // request is over: until the callback reports the outcome, or on a timeout until the closing of
    // the resolver is reported (below). Both outlive the call.
    let code = unsafe {
        WinHttpGetProxyForUrlEx(resolver.0, url.as_ptr(), &raw const options, registration.0)
    };
    let outcome = match code {
        0 => Some(Ok(())),
        ERROR_IO_PENDING => slot.wait(query.timeout),
        other => Some(Err(other)),
    };
    let Some(outcome) = outcome else {
        // Timed out: closing the resolver cancels the request, which may still read `url` and
        // `options` until WinHTTP reports `HANDLE_CLOSING`; wait for that (the registration is
        // still filed) before the buffers go out of scope. If it never comes, leak them: a leak
        // is harmless, a read of freed memory is not.
        drop(resolver);
        if !slot.wait_closed(CLOSE_WAIT) {
            url.leak();
            pac_url.map(Vec::leak);
        }
        return Err(PacError::Timeout);
    };
    if let Err(code) = outcome {
        return Err(if code == ERROR_WINHTTP_OPERATION_CANCELLED {
            PacError::Timeout
        } else {
            failure(code)
        });
    }
    collect(&resolver)
}

fn collect(resolver: &Handle) -> Result<Vec<Hop>, PacError> {
    let mut result = WINHTTP_PROXY_RESULT {
        cEntries: 0,
        pEntries: std::ptr::null_mut(),
    };
    // SAFETY: the resolver completed successfully; `result` is a valid out-structure.
    let code = unsafe { WinHttpGetProxyResult(resolver.0, &raw mut result) };
    if code != 0 {
        return Err(failure(code));
    }
    let mut hops = Vec::new();
    for index in 0..result.cEntries as usize {
        // SAFETY: WinHTTP returned `cEntries` entries starting at `pEntries`.
        let entry = unsafe { &*result.pEntries.add(index) };
        if entry.fProxy == 0 {
            hops.push(Hop::Direct);
        } else if entry.ProxyScheme == WINHTTP_INTERNET_SCHEME_HTTP {
            // SAFETY: `pwszProxy` is null or a terminated string owned by `result`.
            if let Some(host) = unsafe { read_wide(entry.pwszProxy) } {
                hops.push(Hop::Proxy(ProxyAddr::new(&host, entry.ProxyPort)));
            }
        }
    }
    // SAFETY: `result` was filled by WinHttpGetProxyResult and is freed once.
    unsafe { WinHttpFreeProxyResult(&raw mut result) };
    Ok(hops)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests that count `PENDING`'s entries run one at a time: the table is process-wide.
    static COUNTING: Mutex<()> = Mutex::new(());

    fn live() -> usize {
        PENDING.entries().len()
    }

    fn call(context: usize, status: u32, info: *mut c_void) {
        // SAFETY: `context` is a lookup key; `info` is null or a live `WINHTTP_ASYNC_RESULT` for
        // the request-error status, as the callback expects.
        unsafe { callback(std::ptr::null_mut(), context, status, info, 0) };
    }

    #[test]
    fn a_callback_fills_the_slot_of_a_resolution_that_is_waiting() {
        let _serial = COUNTING.lock().unwrap_or_else(PoisonError::into_inner);
        let slot = Arc::new(Slot::default());
        let registration = Registration::new(&slot);
        call(
            registration.0,
            WINHTTP_CALLBACK_STATUS_GETPROXYFORURL_COMPLETE,
            std::ptr::null_mut(),
        );
        assert_eq!(slot.wait(Duration::from_secs(1)), Some(Ok(())));

        let mut failure = WINHTTP_ASYNC_RESULT {
            dwResult: 0,
            dwError: 12002,
        };
        call(
            registration.0,
            WINHTTP_CALLBACK_STATUS_REQUEST_ERROR,
            (&raw mut failure).cast(),
        );
        assert_eq!(slot.wait(Duration::from_secs(1)), Some(Err(12002)));
    }

    #[test]
    fn the_closing_status_releases_a_wait_for_the_handle_to_close() {
        let _serial = COUNTING.lock().unwrap_or_else(PoisonError::into_inner);
        let slot = Arc::new(Slot::default());
        let registration = Registration::new(&slot);
        assert!(!slot.wait_closed(Duration::from_millis(10)));
        call(
            registration.0,
            WINHTTP_CALLBACK_STATUS_HANDLE_CLOSING,
            std::ptr::null_mut(),
        );
        assert!(slot.wait_closed(Duration::from_secs(1)));
        // A closing is not an outcome.
        assert_eq!(slot.wait(Duration::from_millis(10)), None);
    }

    #[test]
    fn a_callback_after_the_resolution_ended_does_nothing() {
        let _serial = COUNTING.lock().unwrap_or_else(PoisonError::into_inner);
        let slot = Arc::new(Slot::default());
        let id = Registration::new(&slot).0; // the registration is gone at once: the resolution ended
        let mut failure = WINHTTP_ASYNC_RESULT {
            dwResult: 0,
            dwError: 12017,
        };
        call(
            id,
            WINHTTP_CALLBACK_STATUS_REQUEST_ERROR,
            (&raw mut failure).cast(),
        );
        call(
            id,
            WINHTTP_CALLBACK_STATUS_GETPROXYFORURL_COMPLETE,
            std::ptr::null_mut(),
        );
        assert_eq!(slot.wait(Duration::from_millis(10)), None);
    }

    #[test]
    fn a_registration_holds_its_entry_until_it_is_dropped() {
        let _serial = COUNTING.lock().unwrap_or_else(PoisonError::into_inner);
        let before = live();
        let slot = Arc::new(Slot::default());
        let first = Registration::new(&slot);
        let second = Registration::new(&slot);
        assert_ne!(first.0, second.0);
        assert_eq!(live(), before + 2);
        drop(first);
        assert_eq!(live(), before + 1);
        assert!(PENDING.find(second.0).is_some());
        drop(second);
        assert_eq!(live(), before);
        // The table's own clones are gone with the entries.
        assert_eq!(Arc::strong_count(&slot), 1);
    }

    #[test]
    fn a_timed_out_resolution_waits_for_the_close_and_ends_as_a_timeout() {
        let _serial = COUNTING.lock().unwrap_or_else(PoisonError::into_inner);
        let before = live();
        // A zero timeout expires before any answer can come, so this takes the timeout path.
        let query = PacQuery {
            pac_url: Some("http://127.0.0.1:1/proxy.pac".to_owned()),
            auto_detect: false,
            url: "http://example.test/".to_owned(),
            timeout: Duration::ZERO,
        };
        for _ in 0..5 {
            let result = resolve(&query);
            assert!(
                matches!(
                    result,
                    Err(PacError::Timeout | PacError::Failed(_) | PacError::Unavailable(_))
                ),
                "{result:?}"
            );
            assert_eq!(live(), before);
        }
    }

    #[test]
    fn resolutions_remove_their_registration() {
        let _serial = COUNTING.lock().unwrap_or_else(PoisonError::into_inner);
        let before = live();
        // Nothing listens on port 1, so each resolution ends in an error or a timeout.
        let query = PacQuery {
            pac_url: Some("http://127.0.0.1:1/proxy.pac".to_owned()),
            auto_detect: false,
            url: "http://example.test/".to_owned(),
            timeout: Duration::from_secs(5),
        };
        for _ in 0..5 {
            let _ = resolve(&query);
            assert_eq!(live(), before);
        }
    }
}
