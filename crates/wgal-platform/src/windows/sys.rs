//! The real [`ServiceControl`] and [`IpHelper`]: thin, safe wrappers around
//! the Win32 calls the Windows adapter needs. This is the only file in the
//! workspace allowed to use `unsafe` (AGENTS.md). Every handle is closed by a
//! guard, every buffer is sized and aligned for the structure written into
//! it, and nothing read from Windows is trusted to stay inside its buffer
//! without a bounds check.
//!
//! Access is the least each call needs: the SCM is opened with
//! `SC_MANAGER_CONNECT` (plus `SC_MANAGER_ENUMERATE_SERVICE` only to list
//! names), and a service only with `SERVICE_QUERY_STATUS`, `SERVICE_START`
//! or `SERVICE_STOP`. Nothing here can create, delete or reconfigure a
//! service, or change an address, route or interface.
//!
//! It also holds the DPAPI wrappers the notification package uses to protect
//! the Telegram bot token at rest ([`protect_secret`], [`unprotect_secret`]),
//! because this is the only file allowed `unsafe`.
//!
//! For the watchdog service it holds the rest of what it needs from Windows:
//! the named pipe server ([`PipeListener`], [`PipeInstance`]) with overlapped
//! I/O under deadlines, the connected client's identity
//! ([`PipeInstance::client_identity`]) and this process's
//! ([`current_identity`]), SID validation ([`is_canonical_sid`]), the
//! ProgramData folder ([`program_data_dir`]), and opening the installation's
//! own files without following reparse points and reading who may change
//! them ([`open_folder_no_follow`], [`open_file_no_follow`],
//! [`object_security`]). For the `wgal` command line it turns the console's
//! echo off while a secret is typed and puts it back, on an interrupt too
//! ([`hide_stdin_echo`], [`restore_stdin_echo`]). For the desktop GUI it holds the client side of the
//! watchdog's pipe ([`connect_watchdog_pipe`]) and where an open file really
//! is ([`final_path_of_file`]). Kernel handles are owned by [`OwnedHandle`],
//! so each is closed exactly once; memory Windows allocates for the caller is
//! freed by a guard; and no `OVERLAPPED` or I/O buffer is released while
//! Windows may still write to it.

#![allow(unsafe_code)]

use std::ffi::OsString;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_BAD_NET_NAME, ERROR_BROKEN_PIPE, ERROR_BUFFER_OVERFLOW,
    ERROR_INVALID_DATA, ERROR_INVALID_OWNER, ERROR_INVALID_PARAMETER, ERROR_INVALID_PRIMARY_GROUP,
    ERROR_IO_PENDING, ERROR_MORE_DATA, ERROR_NO_DATA, ERROR_NOT_FOUND, ERROR_OPERATION_ABORTED,
    ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, ERROR_PRIVILEGE_NOT_HELD, GENERIC_ALL,
    GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE, LocalFree, NO_ERROR, S_OK,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    CancelMibChangeNotify2, FreeMibTable, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER,
    GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses, GetBestRoute2, GetIpForwardTable2,
    GetIpNetEntry2, ICMP_ECHO_REPLY, IP_ADAPTER_ADDRESSES_LH, IP_ADAPTER_UNICAST_ADDRESS_LH,
    IcmpCloseHandle, IcmpCreateFile, IcmpSendEcho2Ex, MIB_IPFORWARD_ROW2, MIB_IPFORWARD_TABLE2,
    MIB_IPINTERFACE_ROW, MIB_IPNET_ROW2, MIB_NOTIFICATION_TYPE, NotifyIpInterfaceChange,
    ResolveIpNetEntry2,
};
use windows_sys::Win32::NetworkManagement::Ndis::{IfOperStatusUp, NET_LUID_LH};
use windows_sys::Win32::Networking::WinSock::{
    ADDRESS_FAMILY, AF_INET, AF_INET6, AF_UNSPEC, IN_ADDR, IN_ADDR_0, IN6_ADDR, IN6_ADDR_0,
    NlnsIncomplete, NlnsPermanent, NlnsUnreachable, SOCKADDR_IN, SOCKADDR_IN6, SOCKADDR_IN6_0,
    SOCKADDR_INET,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertSidToStringSidW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW, ConvertStringSidToSidW, GetSecurityInfo,
    SDDL_REVISION_1, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows_sys::Win32::Security::{
    ACE_HEADER, ACL, CheckTokenMembership, CreateWellKnownSid, DACL_SECURITY_INFORMATION,
    DuplicateToken, GetAce, GetTokenInformation, INHERIT_ONLY_ACE, IsValidSid, IsWellKnownSid,
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, RevertToSelf, SECURITY_ATTRIBUTES,
    SECURITY_MAX_SID_SIZE, SecurityIdentification, TOKEN_DUPLICATE, TOKEN_ELEVATION, TOKEN_QUERY,
    TOKEN_USER, TokenElevation, TokenUser, WELL_KNOWN_SID_TYPE, WinBuiltinAdministratorsSid,
    WinLocalSystemSid,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, DELETE, FILE_APPEND_DATA, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_DELETE_CHILD, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_FIRST_PIPE_INSTANCE,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_FLAG_OVERLAPPED, FILE_GENERIC_READ, FILE_NAME_NORMALIZED,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, FILE_WRITE_EA,
    GetFileInformationByHandle, GetFinalPathNameByHandleW, PIPE_ACCESS_DUPLEX, ReadFile,
    SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT, VOLUME_NAME_DOS, WRITE_DAC, WRITE_OWNER,
    WriteFile,
};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::Console::{
    ENABLE_ECHO_INPUT, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE, SetConsoleCtrlHandler,
    SetConsoleMode,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, ImpersonateNamedPipeClient, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Services::{
    CloseServiceHandle, ControlService, ENUM_SERVICE_STATUS_PROCESSW, EnumServicesStatusExW,
    OpenSCManagerW, OpenServiceW, QueryServiceStatusEx, SC_ENUM_PROCESS_INFO, SC_HANDLE,
    SC_MANAGER_CONNECT, SC_MANAGER_ENUMERATE_SERVICE, SC_STATUS_PROCESS_INFO, SERVICE_CONTROL_STOP,
    SERVICE_QUERY_STATUS, SERVICE_START, SERVICE_STATE_ALL, SERVICE_STATUS, SERVICE_STATUS_PROCESS,
    SERVICE_STOP, SERVICE_WIN32, StartServiceW,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetCurrentThread, INFINITE, OpenProcessToken, OpenThreadToken,
    SetEvent, WaitForMultipleObjects,
};
use windows_sys::Win32::UI::Shell::{FOLDERID_ProgramData, SHGetKnownFolderPath};

use super::local::{DefaultRoute, InterfaceChange, MAX_DEFAULT_ROUTES};
use super::net::status;
use super::{
    AdapterInfo, BestRoute, Control, EchoReply, IpHelper, MAX_ADAPTERS, MAX_ADDRESSES,
    MAX_SERVICES, NetError, ScmError, ServiceControl, ServiceName, ServiceState, lock,
};

/// Longest service or adapter name read, in UTF-16 units (Windows caps both
/// at 256 characters).
const MAX_NAME_UNITS: usize = 256;
/// Largest buffer either enumeration may ask for.
const MAX_BUFFER_BYTES: usize = 4 * 1024 * 1024;
/// Data carried by each echo, checked in the reply.
const ECHO_PAYLOAD: [u8; 32] = *b"WGAL-probe:0123456789abcdefghijk";

fn last_error() -> u32 {
    // SAFETY: GetLastError only reads the calling thread's last-error value.
    unsafe { GetLastError() }
}

/// A buffer of `bytes` bytes, 8-byte aligned for the structures Windows
/// writes into it (they hold pointers and 64-bit values).
fn aligned_buffer(bytes: usize) -> Vec<u64> {
    vec![0_u64; bytes.div_ceil(size_of::<u64>())]
}

fn byte_len(buffer: &[u64]) -> u32 {
    u32::try_from(size_of_val(buffer)).unwrap_or(u32::MAX)
}

/// The address range `[start, end)` that `buffer` occupies.
fn span(buffer: &[u64]) -> (usize, usize) {
    let start = buffer.as_ptr() as usize;
    (start, start + size_of_val(buffer))
}

/// NUL-terminated UTF-16 for a name that has no NUL of its own.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Reads the NUL-terminated UTF-16 string at `text`, which must lie within
/// `buffer`. `None` if the pointer is null or outside the buffer, the string
/// does not end inside the buffer within `MAX_NAME_UNITS`, or it is not valid
/// UTF-16.
fn read_wide(buffer: &[u64], text: *const u16) -> Option<String> {
    let (start, end) = span(buffer);
    let at = text as usize;
    if text.is_null() || at < start || at >= end || !at.is_multiple_of(size_of::<u16>()) {
        return None;
    }
    let available = ((end - at) / size_of::<u16>()).min(MAX_NAME_UNITS + 1);
    // SAFETY: `text` is 2-byte aligned and `available` units from it lie
    // inside `buffer`, which is borrowed for the life of the slice.
    let units = unsafe { std::slice::from_raw_parts(text, available) };
    let len = units.iter().position(|&u| u == 0)?;
    String::from_utf16(&units[..len]).ok()
}

/// An SCM or service handle, closed on drop.
#[derive(Debug)]
struct ScHandle(SC_HANDLE);

impl Drop for ScHandle {
    fn drop(&mut self) {
        // SAFETY: the handle was returned non-null by OpenSCManagerW or
        // OpenServiceW and is closed exactly once, here.
        unsafe {
            CloseServiceHandle(self.0);
        }
    }
}

fn scm_error() -> ScmError {
    ScmError { code: last_error() }
}

fn open_manager(access: u32) -> Result<ScHandle, ScmError> {
    // SAFETY: null machine and database names select the local active
    // services database; there are no other inputs.
    let handle = unsafe { OpenSCManagerW(ptr::null(), ptr::null(), access) };
    if handle.is_null() {
        Err(scm_error())
    } else {
        Ok(ScHandle(handle))
    }
}

/// Opens a service by name with exactly `access`. Private: the trait only
/// reaches it with a [`ServiceName`], the smoke tests with a fixed name.
fn open_service(name: &str, access: u32) -> Result<ScHandle, ScmError> {
    if name.contains('\0') {
        return Err(ScmError::INVALID_NAME);
    }
    let manager = open_manager(SC_MANAGER_CONNECT)?;
    let name = wide(name);
    // SAFETY: `manager` is a live SCM handle and `name` is NUL-terminated
    // UTF-16 that outlives the call.
    let handle = unsafe { OpenServiceW(manager.0, name.as_ptr(), access) };
    if handle.is_null() {
        Err(scm_error())
    } else {
        // The service handle stays valid after the manager handle closes.
        Ok(ScHandle(handle))
    }
}

fn query_service(name: &str) -> Result<ServiceState, ScmError> {
    let service = open_service(name, SERVICE_QUERY_STATUS)?;
    let mut status = SERVICE_STATUS_PROCESS::default();
    let mut needed = 0_u32;
    let size = u32::try_from(size_of::<SERVICE_STATUS_PROCESS>()).unwrap_or(u32::MAX);
    // SAFETY: `service` is live with query access; the buffer is a whole
    // SERVICE_STATUS_PROCESS of the size passed; `needed` is a valid out
    // pointer.
    let ok = unsafe {
        QueryServiceStatusEx(
            service.0,
            SC_STATUS_PROCESS_INFO,
            (&raw mut status).cast::<u8>(),
            size,
            &raw mut needed,
        )
    };
    if ok == 0 {
        return Err(scm_error());
    }
    Ok(ServiceState::from_raw(status.dwCurrentState))
}

fn service_names() -> Result<Vec<String>, ScmError> {
    let manager = open_manager(SC_MANAGER_CONNECT | SC_MANAGER_ENUMERATE_SERVICE)?;
    let mut buffer = aligned_buffer(64 * 1024);
    let mut resume = 0_u32;
    let mut names = Vec::new();
    loop {
        let mut needed = 0_u32;
        let mut returned = 0_u32;
        // SAFETY: `manager` is live with enumerate access; the buffer is
        // writable for the length passed and aligned for
        // ENUM_SERVICE_STATUS_PROCESSW; the out pointers and the resume
        // handle are valid locals; a null group selects every group.
        let ok = unsafe {
            EnumServicesStatusExW(
                manager.0,
                SC_ENUM_PROCESS_INFO,
                SERVICE_WIN32,
                SERVICE_STATE_ALL,
                buffer.as_mut_ptr().cast::<u8>(),
                byte_len(&buffer),
                &raw mut needed,
                &raw mut returned,
                &raw mut resume,
                ptr::null(),
            )
        };
        let more = if ok != 0 {
            false
        } else {
            match last_error() {
                ERROR_MORE_DATA => true,
                code => return Err(ScmError { code }),
            }
        };
        let fits = (byte_len(&buffer) as usize) / size_of::<ENUM_SERVICE_STATUS_PROCESSW>();
        let count = (returned as usize).min(fits);
        let entries = buffer.as_ptr().cast::<ENUM_SERVICE_STATUS_PROCESSW>();
        for i in 0..count {
            // SAFETY: the SCM wrote `returned` consecutive entries at the start
            // of `buffer`, `i` is below both that count and the number that
            // fit, and the buffer is not modified while the entry is read.
            let entry = unsafe { entries.add(i).read() };
            // Names that cannot be read cannot be a tunnel's; skip them.
            if let Some(name) = read_wide(&buffer, entry.lpServiceName) {
                names.push(name);
            }
            if names.len() > MAX_SERVICES {
                return Err(ScmError {
                    code: ERROR_MORE_DATA,
                });
            }
        }
        if !more {
            return Ok(names);
        }
        if returned == 0 {
            // Not even one entry fit: grow to what the SCM asked for.
            let wanted = needed as usize;
            if wanted > MAX_BUFFER_BYTES || wanted <= byte_len(&buffer) as usize {
                return Err(ScmError {
                    code: ERROR_MORE_DATA,
                });
            }
            buffer = aligned_buffer(wanted);
        }
    }
}

/// The SCM on this host. Opens a fresh handle per call.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemServiceControl;

impl ServiceControl for SystemServiceControl {
    fn service_names(&self) -> Result<Vec<String>, ScmError> {
        service_names()
    }

    fn query(&self, service: &ServiceName) -> Result<ServiceState, ScmError> {
        query_service(service.as_str())
    }

    type Handle = ServiceHandle;

    fn open(&self, service: &ServiceName, control: Control) -> Result<ServiceHandle, ScmError> {
        let access = match control {
            Control::Start => SERVICE_START,
            Control::Stop => SERVICE_STOP,
        };
        open_service(service.as_str(), access).map(|handle| ServiceHandle { handle, control })
    }

    fn send(&self, service: &ServiceHandle, control: Control) -> Result<(), ScmError> {
        // A handle opened for one request never carries the other.
        if service.control != control {
            return Err(ScmError::ACCESS_DENIED);
        }
        let ok = match control {
            // SAFETY: the handle is live with start access; zero arguments and
            // a null argument vector start the service as configured.
            Control::Start => unsafe { StartServiceW(service.handle.0, 0, ptr::null()) },
            Control::Stop => {
                let mut status = SERVICE_STATUS::default();
                // SAFETY: the handle is live with stop access and `status` is
                // a valid SERVICE_STATUS for the SCM to fill.
                unsafe { ControlService(service.handle.0, SERVICE_CONTROL_STOP, &raw mut status) }
            }
        };
        if ok == 0 { Err(scm_error()) } else { Ok(()) }
    }
}

/// A tunnel service opened with the access one [`Control`] needs, closed on
/// drop.
#[derive(Debug)]
pub struct ServiceHandle {
    handle: ScHandle,
    control: Control,
}

fn ipv4_sockaddr(address: Ipv4Addr) -> SOCKADDR_INET {
    SOCKADDR_INET {
        Ipv4: SOCKADDR_IN {
            sin_family: AF_INET,
            sin_port: 0,
            sin_addr: IN_ADDR {
                S_un: IN_ADDR_0 {
                    // Network byte order: the octets as they sit in memory.
                    S_addr: u32::from_ne_bytes(address.octets()),
                },
            },
            sin_zero: [0; 8],
        },
    }
}

/// An IPv6 socket address with no port, flow or scope: the interface is
/// always named on its own (by LUID) wherever one is passed.
fn ipv6_sockaddr(address: Ipv6Addr) -> SOCKADDR_INET {
    SOCKADDR_INET {
        Ipv6: SOCKADDR_IN6 {
            sin6_family: AF_INET6,
            sin6_port: 0,
            sin6_flowinfo: 0,
            sin6_addr: IN6_ADDR {
                u: IN6_ADDR_0 {
                    Byte: address.octets(),
                },
            },
            Anonymous: SOCKADDR_IN6_0 { sin6_scope_id: 0 },
        },
    }
}

fn ip_sockaddr(address: IpAddr) -> SOCKADDR_INET {
    match address {
        IpAddr::V4(v4) => ipv4_sockaddr(v4),
        IpAddr::V6(v6) => ipv6_sockaddr(v6),
    }
}

/// Every adapter with its IPv4 addresses; `ipv6` stays empty.
fn adapters() -> Result<Vec<AdapterInfo>, NetError> {
    list_adapters(AF_INET)
}

/// Every adapter with its IPv4 and IPv6 addresses.
fn adapters_any_family() -> Result<Vec<AdapterInfo>, NetError> {
    list_adapters(AF_UNSPEC)
}

fn list_adapters(family: ADDRESS_FAMILY) -> Result<Vec<AdapterInfo>, NetError> {
    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let mut size = 16 * 1024_u32;
    for _ in 0..4 {
        let mut buffer = aligned_buffer(size as usize);
        size = byte_len(&buffer);
        // SAFETY: the buffer is writable for `size` bytes and aligned for
        // IP_ADAPTER_ADDRESSES_LH; `size` is a valid in/out pointer.
        let rc = unsafe {
            GetAdaptersAddresses(
                u32::from(family),
                flags,
                ptr::null(),
                buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>(),
                &raw mut size,
            )
        };
        match rc {
            NO_ERROR => return parse_adapters(&buffer),
            ERROR_NO_DATA => return Ok(Vec::new()),
            ERROR_BUFFER_OVERFLOW if (size as usize) <= MAX_BUFFER_BYTES => {}
            code => return Err(NetError { code }),
        }
    }
    // The table kept growing between calls.
    Err(NetError::BUFFER_OVERFLOW)
}

/// Whether `[at, at + len)` lies inside `buffer`.
fn inside(buffer: &[u64], at: usize, len: usize) -> bool {
    let (start, end) = span(buffer);
    at >= start && at.checked_add(len).is_some_and(|stop| stop <= end)
}

fn parse_adapters(buffer: &[u64]) -> Result<Vec<AdapterInfo>, NetError> {
    let mut adapters = Vec::new();
    let mut node = buffer.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
    while !node.is_null() {
        if adapters.len() >= MAX_ADAPTERS
            || !inside(buffer, node as usize, size_of::<IP_ADAPTER_ADDRESSES_LH>())
        {
            return Err(NetError::BUFFER_OVERFLOW);
        }
        // SAFETY: the whole structure lies inside the buffer
        // GetAdaptersAddresses filled, which is borrowed throughout; it is
        // copied out unaligned.
        let adapter = unsafe { node.read_unaligned() };
        // SAFETY: both union views share the documented (Length, IfIndex)
        // layout for every adapter GetAdaptersAddresses returns.
        let if_index = unsafe { adapter.Anonymous1.Anonymous.IfIndex };
        // SAFETY: NET_LUID_LH is a u64 under every view.
        let luid = unsafe { adapter.Luid.Value };
        let mut ipv4 = Vec::new();
        let mut ipv6 = Vec::new();
        let mut unicast = adapter.FirstUnicastAddress;
        while !unicast.is_null() {
            if ipv4.len() >= MAX_ADDRESSES
                || ipv6.len() >= MAX_ADDRESSES
                || !inside(
                    buffer,
                    unicast as usize,
                    size_of::<IP_ADAPTER_UNICAST_ADDRESS_LH>(),
                )
            {
                return Err(NetError::BUFFER_OVERFLOW);
            }
            // SAFETY: the entry lies inside the buffer (checked above); it is
            // copied out unaligned.
            let entry = unsafe { unicast.read_unaligned() };
            let sockaddr = entry.Address.lpSockaddr;
            let length = usize::try_from(entry.Address.iSockaddrLength).unwrap_or(0);
            let fits = |len: usize| length >= len && inside(buffer, sockaddr as usize, len);
            if fits(size_of::<SOCKADDR_IN>()) {
                // SAFETY: the socket address lies inside the buffer and is at
                // least a SOCKADDR_IN long; it is read unaligned to be safe.
                let ip = unsafe { sockaddr.cast::<SOCKADDR_IN>().read_unaligned() };
                if ip.sin_family == AF_INET {
                    // SAFETY: S_addr is a u32 under every view of the union.
                    let raw = unsafe { ip.sin_addr.S_un.S_addr };
                    ipv4.push(Ipv4Addr::from(raw.to_ne_bytes()));
                } else if ip.sin_family == AF_INET6 && fits(size_of::<SOCKADDR_IN6>()) {
                    // SAFETY: the socket address lies inside the buffer, is at
                    // least a SOCKADDR_IN6 long and says it is IPv6; it is read
                    // unaligned to be safe.
                    let ip = unsafe { sockaddr.cast::<SOCKADDR_IN6>().read_unaligned() };
                    // SAFETY: Byte is a [u8; 16] under every view of the union.
                    let bytes = unsafe { ip.sin6_addr.u.Byte };
                    ipv6.push(Ipv6Addr::from(bytes));
                }
            }
            unicast = entry.Next;
        }
        adapters.push(AdapterInfo {
            // An unreadable name cannot match a tunnel name.
            friendly_name: read_wide(buffer, adapter.FriendlyName).unwrap_or_default(),
            if_type: adapter.IfType,
            if_index,
            luid,
            up: adapter.OperStatus == IfOperStatusUp,
            ipv4,
            ipv6,
        });
        node = adapter.Next;
    }
    Ok(adapters)
}

fn best_route(destination: Ipv4Addr) -> Result<BestRoute, NetError> {
    let destination = ipv4_sockaddr(destination);
    let mut row = MIB_IPFORWARD_ROW2::default();
    let mut source = SOCKADDR_INET::default();
    // SAFETY: every pointer refers to an initialized local that outlives the
    // call; a null LUID, zero index and null source let Windows choose any
    // interface and source, which is the answer being checked.
    let rc = unsafe {
        GetBestRoute2(
            ptr::null(),
            0,
            ptr::null(),
            &raw const destination,
            0,
            &raw mut row,
            &raw mut source,
        )
    };
    if rc != NO_ERROR {
        return Err(NetError { code: rc });
    }
    // SAFETY: si_family is the leading u16 of every view of the union.
    if unsafe { source.si_family } != AF_INET {
        return Err(NetError::INVALID_PARAMETER);
    }
    // SAFETY: the family is AF_INET, so the IPv4 view is the one written.
    let raw = unsafe { source.Ipv4.sin_addr.S_un.S_addr };
    Ok(BestRoute {
        if_index: row.InterfaceIndex,
        // SAFETY: NET_LUID_LH is a u64 under every view.
        luid: unsafe { row.InterfaceLuid.Value },
        source: Ipv4Addr::from(raw.to_ne_bytes()),
    })
}

/// An ICMP handle, closed on drop.
#[derive(Debug)]
struct IcmpHandle(HANDLE);

impl Drop for IcmpHandle {
    fn drop(&mut self) {
        // SAFETY: the handle came from IcmpCreateFile, was valid, and is
        // closed exactly once, here.
        unsafe {
            IcmpCloseHandle(self.0);
        }
    }
}

fn echo(source: Ipv4Addr, destination: Ipv4Addr, timeout: Duration) -> Result<EchoReply, NetError> {
    // SAFETY: IcmpCreateFile has no inputs.
    let handle = unsafe { IcmpCreateFile() };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return Err(NetError { code: last_error() });
    }
    let handle = IcmpHandle(handle);
    // Room for the reply structure, the echoed data, an ICMP error and the
    // IO_STATUS_BLOCK the API may append.
    let mut reply = aligned_buffer(size_of::<ICMP_ECHO_REPLY>() + ECHO_PAYLOAD.len() + 256);
    let timeout_ms = u32::try_from(timeout.as_millis())
        .unwrap_or(u32::MAX)
        .max(1);
    let payload_len = u16::try_from(ECHO_PAYLOAD.len()).unwrap_or(u16::MAX);
    // SAFETY: `handle` is live; a null event and no APC make the call
    // synchronous, bounded by `timeout_ms`; the payload and reply buffers are
    // valid for the sizes passed and outlive the call; options are null.
    let count = unsafe {
        IcmpSendEcho2Ex(
            handle.0,
            ptr::null_mut(),
            None,
            ptr::null(),
            u32::from_ne_bytes(source.octets()),
            u32::from_ne_bytes(destination.octets()),
            ECHO_PAYLOAD.as_ptr().cast(),
            payload_len,
            ptr::null(),
            reply.as_mut_ptr().cast(),
            byte_len(&reply),
            timeout_ms,
        )
    };
    if count == 0 {
        let code = last_error();
        return if status::is_ip_status(code) {
            Ok(EchoReply {
                status: code,
                from: Ipv4Addr::UNSPECIFIED,
                rtt_ms: 0,
                payload_matches: false,
            })
        } else {
            Err(NetError { code })
        };
    }
    // SAFETY: at least one reply was written, so the aligned buffer starts
    // with an ICMP_ECHO_REPLY.
    let first = unsafe { reply.as_ptr().cast::<ICMP_ECHO_REPLY>().read() };
    let data = first.Data.cast::<u8>().cast_const();
    let size = usize::from(first.DataSize);
    let payload_matches = size == ECHO_PAYLOAD.len()
        && !data.is_null()
        && inside(&reply, data as usize, size)
        // SAFETY: `size` bytes at `data` lie inside the reply buffer (checked
        // just above), which is borrowed for the life of the slice.
        && unsafe { std::slice::from_raw_parts(data, size) } == ECHO_PAYLOAD;
    Ok(EchoReply {
        status: first.Status,
        from: Ipv4Addr::from(first.Address.to_ne_bytes()),
        rtt_ms: first.RoundTripTime,
        payload_matches,
    })
}

/// The IP Helper API on this host.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemIpHelper;

impl IpHelper for SystemIpHelper {
    fn adapters(&self) -> Result<Vec<AdapterInfo>, NetError> {
        adapters()
    }

    fn adapters_any_family(&self) -> Result<Vec<AdapterInfo>, NetError> {
        adapters_any_family()
    }

    fn best_route(&self, destination: Ipv4Addr) -> Result<BestRoute, NetError> {
        best_route(destination)
    }

    fn echo(
        &self,
        source: Ipv4Addr,
        destination: Ipv4Addr,
        timeout: Duration,
    ) -> Result<EchoReply, NetError> {
        echo(source, destination, timeout)
    }

    fn default_routes(&self) -> Result<Vec<DefaultRoute>, NetError> {
        default_routes()
    }

    fn neighbor_is_static(&self, luid: u64, address: IpAddr) -> Result<bool, NetError> {
        neighbor_is_static(luid, address)
    }

    fn resolve_neighbor(&self, luid: u64, address: IpAddr) -> Result<bool, NetError> {
        resolve_neighbor(luid, address)
    }
}

/// Most rows of the routing table read; a larger table fails instead of
/// being read in part.
const MAX_ROUTE_ROWS: usize = 1 << 20;

/// A table the IP Helper API allocated, freed on drop.
#[derive(Debug)]
struct MibTable(*mut MIB_IPFORWARD_TABLE2);

impl Drop for MibTable {
    fn drop(&mut self) {
        // SAFETY: the pointer came non-null from GetIpForwardTable2, which
        // allocated it for FreeMibTable, and is freed exactly once, here.
        unsafe {
            FreeMibTable(self.0.cast_const().cast());
        }
    }
}

/// The IPv4 or IPv6 address in a `SOCKADDR_INET`, if it holds one.
fn inet_ip(address: &SOCKADDR_INET) -> Option<IpAddr> {
    // SAFETY: si_family is the leading u16 of every view of the union.
    let family = unsafe { address.si_family };
    if family == AF_INET {
        // SAFETY: the family is AF_INET, so the IPv4 view is the one written.
        let raw = unsafe { address.Ipv4.sin_addr.S_un.S_addr };
        Some(IpAddr::V4(Ipv4Addr::from(raw.to_ne_bytes())))
    } else if family == AF_INET6 {
        // SAFETY: the family is AF_INET6, so the IPv6 view is the one
        // written; Byte is a [u8; 16] under every view of its union.
        let bytes = unsafe { address.Ipv6.sin6_addr.u.Byte };
        Some(IpAddr::V6(Ipv6Addr::from(bytes)))
    } else {
        None
    }
}

/// Every `0.0.0.0/0` and `::/0` route, read from the routing tables of both
/// families at once.
fn default_routes() -> Result<Vec<DefaultRoute>, NetError> {
    let mut table: *mut MIB_IPFORWARD_TABLE2 = ptr::null_mut();
    // SAFETY: `table` is a valid out pointer; on success Windows stores a
    // table it allocated there, freed by the guard below.
    let rc = unsafe { GetIpForwardTable2(AF_UNSPEC, &raw mut table) };
    match rc {
        NO_ERROR => {}
        // No routes at all.
        ERROR_NOT_FOUND => return Ok(Vec::new()),
        code => return Err(NetError { code }),
    }
    if table.is_null() {
        return Err(NetError::INVALID_PARAMETER);
    }
    let table = MibTable(table);
    // SAFETY: the table is live until the guard drops; NumEntries is its
    // first field.
    let count = unsafe { (*table.0).NumEntries } as usize;
    if count > MAX_ROUTE_ROWS {
        return Err(NetError::BUFFER_OVERFLOW);
    }
    // SAFETY: a field address of the live table; no reference is made.
    let first = unsafe { (&raw const (*table.0).Table).cast::<MIB_IPFORWARD_ROW2>() };
    let mut routes = Vec::new();
    for at in 0..count {
        // SAFETY: Windows allocated the table with `count` rows laid out as
        // a C array from `Table`, so row `at` lies inside it; it is copied
        // out unaligned, and the table outlives the copy.
        let row = unsafe { first.add(at).read_unaligned() };
        let prefix = &row.DestinationPrefix;
        let is_default =
            prefix.PrefixLength == 0 && inet_ip(&prefix.Prefix).is_some_and(|a| a.is_unspecified());
        if !is_default {
            continue;
        }
        let Some(next_hop) = inet_ip(&row.NextHop) else {
            continue;
        };
        if routes.len() >= MAX_DEFAULT_ROUTES {
            return Err(NetError::BUFFER_OVERFLOW);
        }
        routes.push(DefaultRoute {
            if_index: row.InterfaceIndex,
            // SAFETY: NET_LUID_LH is a u64 under every view.
            luid: unsafe { row.InterfaceLuid.Value },
            next_hop,
        });
    }
    Ok(routes)
}

/// A neighbor row naming `address` on the interface `luid`, the rest zero.
/// The interface is named by its LUID, so an IPv6 link-local gateway needs
/// no scope in the address.
fn neighbor_row(luid: u64, address: IpAddr) -> MIB_IPNET_ROW2 {
    MIB_IPNET_ROW2 {
        Address: ip_sockaddr(address),
        InterfaceLuid: NET_LUID_LH { Value: luid },
        ..MIB_IPNET_ROW2::default()
    }
}

/// Reads the neighbor entry for `address` on `luid`; only reads.
fn neighbor_is_static(luid: u64, address: IpAddr) -> Result<bool, NetError> {
    let mut row = neighbor_row(luid, address);
    // SAFETY: `row` is an initialized local that outlives the call, with the
    // address and interface set as GetIpNetEntry2 requires; it writes only
    // into the row.
    let rc = unsafe { GetIpNetEntry2(&raw mut row) };
    match rc {
        NO_ERROR => Ok(row.State == NlnsPermanent),
        ERROR_NOT_FOUND => Ok(false),
        code => Err(NetError { code }),
    }
}

/// Resolves `address` on `luid` afresh (see [`IpHelper::resolve_neighbor`]).
fn resolve_neighbor(luid: u64, address: IpAddr) -> Result<bool, NetError> {
    let mut row = neighbor_row(luid, address);
    // SAFETY: `row` is an initialized local that outlives the call, with the
    // address and interface set as ResolveIpNetEntry2 requires; a null
    // source lets Windows send from the interface's own address.
    let rc = unsafe { ResolveIpNetEntry2(&raw mut row, ptr::null()) };
    match rc {
        // Success without a link-layer address is no answer.
        NO_ERROR => Ok(row.State != NlnsUnreachable
            && row.State != NlnsIncomplete
            && row.PhysicalAddressLength > 0),
        ERROR_BAD_NET_NAME => Ok(false),
        code => Err(NetError { code }),
    }
}

/// Room for this many notices between two reads of the channel; more are
/// dropped, since the receiver only needs to know that something changed.
const NOTICE_QUEUE: usize = 64;

/// What a registration's callback reaches: the sending end of the channel,
/// taken out when the registration is cancelled so the receiver sees the
/// channel close.
#[derive(Debug)]
struct WatchContext {
    to: Mutex<Option<SyncSender<InterfaceChange>>>,
}

/// Called by Windows, one notice at a time, on a thread of its own.
extern "system" fn on_interface_change(
    context: *const core::ffi::c_void,
    row: *const MIB_IPINTERFACE_ROW,
    _kind: MIB_NOTIFICATION_TYPE,
) {
    if context.is_null() || row.is_null() {
        return;
    }
    // SAFETY: `context` is the `&'static WatchContext` registered with this
    // callback; it is never freed (see `InterfaceWatch`).
    let context = unsafe { &*context.cast::<WatchContext>() };
    // SAFETY: Windows passes a row that is valid for the duration of the
    // call; only its LUID is read, by value.
    let luid = unsafe { (*row).InterfaceLuid.Value };
    if let Some(to) = lock(&context.to).as_ref() {
        // A full queue already tells the receiver that something changed;
        // never block Windows' thread.
        let _ = to.try_send(InterfaceChange { luid });
    }
}

/// A registration for notices of changes to the host's IPv4 interfaces
/// (`NotifyIpInterfaceChange`): each one is sent as an [`InterfaceChange`]
/// on the channel [`InterfaceWatch::start`] returns, without blocking and
/// without looking anything up; when the channel is full, further notices
/// are dropped until it is read. Dropping the watch cancels the
/// registration (`CancelMibChangeNotify2`) and closes the channel.
///
/// The callback's context is left allocated for the life of the process:
/// `CancelMibChangeNotify2` is documented to deadlock when it would wait for
/// a callback, which says it waits for one in progress, but not in so many
/// words; a few bytes per registration are cheaper than a use after free if
/// it did not. Registering changes nothing on the host; notices stop when
/// the process ends in any case.
#[derive(Debug)]
pub struct InterfaceWatch {
    handle: HANDLE,
    context: &'static WatchContext,
}

// SAFETY: the registration handle is not tied to the thread that made it:
// CancelMibChangeNotify2 may be called from any thread but a callback's, and
// `drop` never runs on one. The context is `'static` and `Sync`.
unsafe impl Send for InterfaceWatch {}

impl InterfaceWatch {
    /// Registers for IPv4 interface changes and returns the channel the
    /// notices arrive on. No notice is sent for the registration itself.
    pub fn start() -> Result<(Self, Receiver<InterfaceChange>), Win32Error> {
        let (to, notices) = mpsc::sync_channel(NOTICE_QUEUE);
        let context: &'static WatchContext = Box::leak(Box::new(WatchContext {
            to: Mutex::new(Some(to)),
        }));
        let mut handle: HANDLE = ptr::null_mut();
        // SAFETY: the callback has the documented signature; the context is
        // `'static`; `handle` is a valid out pointer; no initial notice.
        let rc = unsafe {
            NotifyIpInterfaceChange(
                AF_INET,
                Some(on_interface_change),
                ptr::from_ref(context).cast(),
                false,
                &raw mut handle,
            )
        };
        if rc != NO_ERROR || handle.is_null() {
            lock(&context.to).take();
            return Err(Win32Error {
                code: if rc == NO_ERROR {
                    ERROR_INVALID_PARAMETER
                } else {
                    rc
                },
            });
        }
        Ok((Self { handle, context }, notices))
    }
}

impl Drop for InterfaceWatch {
    fn drop(&mut self) {
        // SAFETY: the handle came non-null from NotifyIpInterfaceChange and is
        // cancelled exactly once, here, never from the callback.
        unsafe {
            CancelMibChangeNotify2(self.handle);
        }
        lock(&self.context.to).take();
    }
}

/// Largest secret either DPAPI direction accepts or returns. A bot token is
/// under 100 bytes; its protected form is a few hundred.
const MAX_SECRET_BYTES: usize = 16 * 1024;
/// Ties the protected bytes to this use, so a blob protected by another
/// program for the same account does not unprotect here by accident.
const SECRET_ENTROPY: &[u8] = b"wg-always-live/telegram-bot-token/v1";

/// A secret could not be protected or unprotected: the Windows error code, or
/// 0 when the input or output was out of bounds. Never the secret.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SecretProtectionError {
    pub code: u32,
}

/// Frees a buffer DPAPI allocated, after overwriting it.
struct LocalBuffer {
    ptr: *mut u8,
    len: usize,
}

impl Drop for LocalBuffer {
    fn drop(&mut self) {
        if self.ptr.is_null() {
            return;
        }
        // SAFETY: `ptr` and `len` are exactly the buffer DPAPI returned in
        // `CRYPT_INTEGER_BLOB::pbData`/`cbData`, which this process owns until
        // `LocalFree`; it is overwritten in place and freed exactly once.
        unsafe {
            ptr::write_bytes(self.ptr, 0, self.len);
            LocalFree(self.ptr.cast());
        }
    }
}

fn blob(bytes: &[u8]) -> Result<CRYPT_INTEGER_BLOB, SecretProtectionError> {
    if bytes.len() > MAX_SECRET_BYTES {
        return Err(SecretProtectionError { code: 0 });
    }
    Ok(CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(bytes.len()).map_err(|_| SecretProtectionError { code: 0 })?,
        // DPAPI only reads input blobs; the pointer is never written through.
        pbData: bytes.as_ptr().cast_mut(),
    })
}

/// Protects `plain` with DPAPI for the account this process runs as (for the
/// watchdog service, LocalSystem), without any UI. Only the same account on
/// the same machine can unprotect it.
pub fn protect_secret(plain: &[u8]) -> Result<Vec<u8>, SecretProtectionError> {
    dpapi(plain, true)
}

/// The reverse of [`protect_secret`]. Fails for anything not protected by
/// this account for this use.
pub fn unprotect_secret(sealed: &[u8]) -> Result<Vec<u8>, SecretProtectionError> {
    dpapi(sealed, false)
}

fn dpapi(input: &[u8], protect: bool) -> Result<Vec<u8>, SecretProtectionError> {
    let data_in = blob(input)?;
    let entropy = blob(SECRET_ENTROPY)?;
    let mut data_out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };
    let ok = if protect {
        // SAFETY: every pointer is to a live, correctly sized
        // `CRYPT_INTEGER_BLOB` whose data outlives the call; the description,
        // reserved and prompt pointers may be null; DPAPI writes only to
        // `data_out`, which this function frees through `LocalBuffer`.
        unsafe {
            CryptProtectData(
                &data_in,
                ptr::null(),
                &entropy,
                ptr::null(),
                ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut data_out,
            )
        }
    } else {
        // SAFETY: as for `CryptProtectData`; the description out-pointer is
        // null, so DPAPI allocates nothing else for the caller to free.
        unsafe {
            CryptUnprotectData(
                &data_in,
                ptr::null_mut(),
                &entropy,
                ptr::null(),
                ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut data_out,
            )
        }
    };
    if ok == 0 {
        return Err(SecretProtectionError { code: last_error() });
    }
    let len = usize::try_from(data_out.cbData).unwrap_or(usize::MAX);
    let owned = LocalBuffer {
        ptr: data_out.pbData,
        len: if data_out.pbData.is_null() { 0 } else { len },
    };
    if owned.ptr.is_null() || owned.len > MAX_SECRET_BYTES {
        return Err(SecretProtectionError { code: 0 });
    }
    // SAFETY: DPAPI returned `len` initialized bytes at `ptr`, owned by this
    // process until `owned` frees them below; the slice is copied out first.
    let bytes = unsafe { std::slice::from_raw_parts(owned.ptr, owned.len) }.to_vec();
    drop(owned);
    Ok(bytes)
}

/// A Win32 call failed: its error code, or for the known-folder lookup its
/// `HRESULT`. Never carries text from the caller or from Windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Win32Error {
    pub code: u32,
}

impl Win32Error {
    const INVALID_DATA: Self = Self {
        code: ERROR_INVALID_DATA,
    };
    const INVALID_PARAMETER: Self = Self {
        code: ERROR_INVALID_PARAMETER,
    };

    fn last() -> Self {
        Self { code: last_error() }
    }
}

impl fmt::Display for Win32Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Windows error {}", self.code)
    }
}

impl std::error::Error for Win32Error {}

fn size_u32<T>() -> u32 {
    u32::try_from(size_of::<T>()).unwrap_or(u32::MAX)
}

/// Memory a Windows call allocated with `LocalAlloc` for the caller, freed
/// on drop.
struct LocalMemory(*mut core::ffi::c_void);

impl Drop for LocalMemory {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from a Windows call that hands the
            // caller a LocalAlloc block, and is freed exactly once, here.
            unsafe {
                LocalFree(self.0);
            }
        }
    }
}

/// Longest NUL-terminated string read back from Windows, in UTF-16 units.
/// SID strings stay under 200 and paths under 32,767.
const MAX_RETURNED_UNITS: usize = 32 * 1024;

/// A NUL-terminated UTF-16 string a Windows call allocated for the caller,
/// freed on drop the way that call requires.
struct ReturnedText {
    text: *mut u16,
    /// `CoTaskMemFree` (the known-folder lookup) instead of `LocalFree`.
    co_task: bool,
}

impl ReturnedText {
    /// The units before the terminating NUL; `None` if there is no string
    /// or it is longer than [`MAX_RETURNED_UNITS`].
    fn units(&self) -> Option<&[u16]> {
        if self.text.is_null() {
            return None;
        }
        let mut len = 0;
        loop {
            if len == MAX_RETURNED_UNITS {
                return None;
            }
            // SAFETY: Windows returned a NUL-terminated string at `text`,
            // allocated until `self` drops; reading stops at the first NUL,
            // so `len` never passes the terminator.
            let unit = unsafe { self.text.add(len).read() };
            if unit == 0 {
                break;
            }
            len += 1;
        }
        // SAFETY: the `len` units just read lie before the terminator in the
        // same allocation, which outlives the borrow of `self`.
        Some(unsafe { std::slice::from_raw_parts(self.text, len) })
    }

    fn string(&self) -> Result<String, Win32Error> {
        self.units()
            .and_then(|units| String::from_utf16(units).ok())
            .ok_or(Win32Error::INVALID_DATA)
    }
}

impl Drop for ReturnedText {
    fn drop(&mut self) {
        if self.text.is_null() {
            return;
        }
        if self.co_task {
            // SAFETY: SHGetKnownFolderPath allocated the string with
            // CoTaskMemAlloc; it is freed exactly once, here.
            unsafe { CoTaskMemFree(self.text.cast_const().cast()) }
        } else {
            drop(LocalMemory(self.text.cast()));
        }
    }
}

/// Longest SID string accepted: `S-1-`, an authority of up to 15 digits and
/// at most 15 sub-authorities of up to 10 digits each.
pub const MAX_SID_TEXT_LEN: usize = 4 + 15 + 15 * 11;

/// Size of a SID's fixed part: revision, sub-authority count, authority.
const SID_HEADER_BYTES: usize = 8;

/// Whether a whole, valid SID lies in `[start, end)` at `sid`. Reads nothing
/// outside that range, which the caller must have borrowed.
fn sid_within(sid: PSID, start: usize, end: usize) -> bool {
    let at = sid as usize;
    if sid.is_null()
        || at < start
        || at
            .checked_add(SID_HEADER_BYTES)
            .is_none_or(|header| header > end)
    {
        return false;
    }
    // SAFETY: the SID's fixed part lies inside `[start, end)` (checked
    // above); its second byte is the sub-authority count.
    let count = unsafe { sid.cast::<u8>().add(1).read() };
    let len = SID_HEADER_BYTES + 4 * usize::from(count);
    if at.checked_add(len).is_none_or(|stop| stop > end) {
        return false;
    }
    // SAFETY: the whole SID, `len` bytes by its own count, lies inside the
    // borrowed range.
    unsafe { IsValidSid(sid) != 0 }
}

/// The canonical string for a SID the caller has checked to be valid.
fn sid_text(sid: PSID) -> Result<String, Win32Error> {
    let mut text = ptr::null_mut();
    // SAFETY: `sid` is a valid SID and `text` a valid out pointer.
    let ok = unsafe { ConvertSidToStringSidW(sid, &raw mut text) };
    if ok == 0 {
        return Err(Win32Error::last());
    }
    ReturnedText {
        text,
        co_task: false,
    }
    .string()
}

/// Whether `text` is a SID in the one form Windows itself prints: `S-1-`
/// then decimal fields without leading zeros. It must convert to a valid SID
/// (`ConvertStringSidToSidW`) and back (`ConvertSidToStringSidW`) to exactly
/// the same text, so SDDL aliases such as `BA`, other number formats and
/// anything else that only converts one way are refused.
pub fn is_canonical_sid(text: &str) -> bool {
    let shape = text.len() <= MAX_SID_TEXT_LEN
        && text.strip_prefix("S-1-").is_some_and(|rest| {
            !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit() || b == b'-')
        });
    if !shape {
        return false;
    }
    let text_wide = wide(text);
    let mut sid = ptr::null_mut();
    // SAFETY: `text_wide` is NUL-terminated UTF-16 that outlives the call;
    // `sid` is a valid out pointer.
    if unsafe { ConvertStringSidToSidW(text_wide.as_ptr(), &raw mut sid) } == 0 {
        return false;
    }
    let sid = LocalMemory(sid);
    // SAFETY: ConvertStringSidToSidW returned this SID; it stays allocated
    // until `sid` drops.
    let valid = unsafe { IsValidSid(sid.0) } != 0;
    valid && sid_text(sid.0).is_ok_and(|back| back == text)
}

/// A well-known builtin or local SID, in a buffer of its own.
fn well_known_sid(kind: WELL_KNOWN_SID_TYPE) -> Result<Vec<u64>, Win32Error> {
    let mut buffer = aligned_buffer(SECURITY_MAX_SID_SIZE as usize);
    let mut size = byte_len(&buffer);
    // SAFETY: the buffer is writable for `size` bytes, enough for any SID;
    // the builtin and local SIDs asked for here take no domain SID.
    let ok = unsafe {
        CreateWellKnownSid(
            kind,
            ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &raw mut size,
        )
    };
    if ok == 0 {
        Err(Win32Error::last())
    } else {
        Ok(buffer)
    }
}

/// Whether `sid` is SYSTEM or Administrators.
fn is_privileged_sid(sid: PSID) -> bool {
    // SAFETY: the caller passes a SID it has checked to be valid and keeps
    // it allocated during the call.
    unsafe {
        IsWellKnownSid(sid, WinLocalSystemSid) != 0
            || IsWellKnownSid(sid, WinBuiltinAdministratorsSid) != 0
    }
}

/// What the watchdog needs to know about the account behind a token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenIdentity {
    /// The token's user as a canonical SID string.
    pub sid: String,
    /// The user is LocalSystem.
    pub local_system: bool,
    /// `TokenElevation`: UAC's full, elevated token.
    pub elevated: bool,
    /// The Administrators group is enabled in the token, not deny-only as
    /// in a UAC-filtered token (`CheckTokenMembership`).
    pub administrator: bool,
}

/// Room for a `TOKEN_USER` and its SID (at most 68 bytes).
const TOKEN_USER_BYTES: usize = 256;

/// Reads the identity from an impersonation token opened with `TOKEN_QUERY`:
/// a pipe client's, or a duplicate of this process's token.
fn token_identity(token: &OwnedHandle) -> Result<TokenIdentity, Win32Error> {
    let raw = token.as_raw_handle();
    let mut user = aligned_buffer(TOKEN_USER_BYTES);
    let mut needed = 0_u32;
    // SAFETY: `raw` is a live token with query access; the buffer is
    // writable for the length passed and aligned for TOKEN_USER; `needed` is
    // a valid out pointer.
    let ok = unsafe {
        GetTokenInformation(
            raw,
            TokenUser,
            user.as_mut_ptr().cast(),
            byte_len(&user),
            &raw mut needed,
        )
    };
    if ok == 0 {
        return Err(Win32Error::last());
    }
    // SAFETY: the call succeeded, so the aligned buffer starts with a
    // TOKEN_USER.
    let sid = unsafe { user.as_ptr().cast::<TOKEN_USER>().read() }
        .User
        .Sid;
    let (start, end) = span(&user);
    if !sid_within(sid, start, end) {
        return Err(Win32Error::INVALID_DATA);
    }
    let text = sid_text(sid)?;
    // SAFETY: `sid` is a whole, valid SID inside `user`, still borrowed.
    let local_system = unsafe { IsWellKnownSid(sid, WinLocalSystemSid) } != 0;

    let mut elevation = TOKEN_ELEVATION::default();
    // SAFETY: as above, into a whole TOKEN_ELEVATION of the size passed.
    let ok = unsafe {
        GetTokenInformation(
            raw,
            TokenElevation,
            (&raw mut elevation).cast(),
            size_u32::<TOKEN_ELEVATION>(),
            &raw mut needed,
        )
    };
    if ok == 0 {
        return Err(Win32Error::last());
    }

    let admins = well_known_sid(WinBuiltinAdministratorsSid)?;
    let mut member = 0;
    // SAFETY: `raw` is an impersonation token with query access, as
    // CheckTokenMembership requires; `admins` holds a valid SID that the
    // call only reads; `member` is a valid out pointer.
    let ok =
        unsafe { CheckTokenMembership(raw, admins.as_ptr().cast_mut().cast(), &raw mut member) };
    if ok == 0 {
        return Err(Win32Error::last());
    }
    Ok(TokenIdentity {
        sid: text,
        local_system,
        elevated: elevation.TokenIsElevated != 0,
        administrator: member != 0,
    })
}

/// The identity this process runs as, read the same way as a pipe client's
/// (from an identification-level duplicate of the process token).
pub fn current_identity() -> Result<TokenIdentity, Win32Error> {
    let mut primary = ptr::null_mut();
    // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no
    // closing; `primary` is a valid out pointer.
    let ok = unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY | TOKEN_DUPLICATE,
            &raw mut primary,
        )
    };
    if ok == 0 {
        return Err(Win32Error::last());
    }
    // SAFETY: OpenProcessToken returned a new handle that only this guard
    // owns and closes.
    let primary = unsafe { OwnedHandle::from_raw_handle(primary) };
    let mut duplicate = ptr::null_mut();
    // SAFETY: `primary` is live with duplicate access; `duplicate` is a
    // valid out pointer for the new impersonation token.
    let ok = unsafe {
        DuplicateToken(
            primary.as_raw_handle(),
            SecurityIdentification,
            &raw mut duplicate,
        )
    };
    if ok == 0 {
        return Err(Win32Error::last());
    }
    // SAFETY: DuplicateToken returned a new handle that only this guard owns
    // and closes.
    let duplicate = unsafe { OwnedHandle::from_raw_handle(duplicate) };
    token_identity(&duplicate)
}

/// Ends the calling thread's impersonation of a pipe client when dropped.
struct Impersonating;

impl Drop for Impersonating {
    fn drop(&mut self) {
        // SAFETY: RevertToSelf takes no inputs; it ends the impersonation
        // this guard was created for.
        if unsafe { RevertToSelf() } == 0 {
            // The thread would go on running with the client's token, and
            // for the watchdog service whatever it did next would be checked
            // against the wrong account. Nothing can repair the thread, so
            // the process stops here, before it runs anything else; the
            // service manager restarts it.
            std::process::abort();
        }
    }
}

/// Most instances one pipe may have.
pub const MAX_PIPE_INSTANCES: u32 = 16;
/// Every pipe name served starts with this; the rest is 1 to
/// [`MAX_PIPE_NAME_LEN`] of `[A-Za-z0-9._-]`.
pub const PIPE_NAME_PREFIX: &str = r"\\.\pipe\";
pub const MAX_PIPE_NAME_LEN: usize = 200;
/// What Windows buffers in each direction of an instance: a whole frame of
/// the largest message (advisory; Windows grows it when needed).
const PIPE_BUFFER_BYTES: u32 = 68 * 1024;
/// Longest SDDL string or security descriptor accepted for a pipe.
const MAX_SECURITY_BYTES: usize = 16 * 1024;
/// Unexpected connect failures in a row after which `accept` gives up.
const MAX_CONNECT_FAILURES: u32 = 3;

/// The access a pipe client asks for: read, write data, `READ_CONTROL` (to
/// check who owns the pipe) and `SYNCHRONIZE`. Not `FILE_APPEND_DATA`, which
/// on a pipe is `FILE_CREATE_PIPE_INSTANCE`, and not `WRITE_DAC` or
/// `WRITE_OWNER`. The pipe's DACL grants listed users exactly this.
pub const PIPE_CLIENT_ACCESS: u32 = FILE_GENERIC_READ | FILE_WRITE_DATA;

/// The security quality of service a client opens the pipe with: the server
/// may identify the client but never act as it, so a process squatting the
/// pipe's name gains no use of the client's token.
pub const PIPE_CLIENT_QOS: u32 = SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION;

/// Why a pipe could not be served.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PipeError {
    /// Not [`PIPE_NAME_PREFIX`] followed by 1 to [`MAX_PIPE_NAME_LEN`] of
    /// `[A-Za-z0-9._-]`.
    InvalidName,
    /// An instance limit outside 1 to [`MAX_PIPE_INSTANCES`].
    InvalidLimit,
    /// Windows refused the SDDL, or could not apply the owner it names.
    InvalidSecurity(Win32Error),
    /// A pipe with this name already exists: another server, or a process
    /// squatting the name, holds it.
    NameInUse,
    Os(Win32Error),
}

impl fmt::Display for PipeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName => write!(f, "not a pipe name WGAL serves"),
            Self::InvalidLimit => write!(f, "pipe instance limit out of range"),
            Self::InvalidSecurity(e) => write!(f, "the pipe's security descriptor: {e}"),
            Self::NameInUse => write!(f, "another process already holds the pipe name"),
            Self::Os(e) => write!(f, "the pipe: {e}"),
        }
    }
}

impl std::error::Error for PipeError {}

fn valid_pipe_name(name: &str) -> bool {
    name.strip_prefix(PIPE_NAME_PREFIX).is_some_and(|rest| {
        !rest.is_empty()
            && rest.len() <= MAX_PIPE_NAME_LEN
            && rest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    })
}

/// The self-relative security descriptor for `sddl`, copied out of the
/// block Windows allocated (it holds offsets, not pointers, so the copy is
/// complete).
fn security_descriptor(sddl: &str) -> Result<Vec<u64>, Win32Error> {
    if sddl.is_empty() || sddl.len() > MAX_SECURITY_BYTES || sddl.contains('\0') {
        return Err(Win32Error::INVALID_PARAMETER);
    }
    let text = wide(sddl);
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    let mut size = 0_u32;
    // SAFETY: `text` is NUL-terminated UTF-16 that outlives the call; the
    // out pointers are valid locals.
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text.as_ptr(),
            SDDL_REVISION_1,
            &raw mut descriptor,
            &raw mut size,
        )
    };
    if ok == 0 {
        return Err(Win32Error::last());
    }
    let owned = LocalMemory(descriptor);
    let len = size as usize;
    if owned.0.is_null() || len == 0 || len > MAX_SECURITY_BYTES {
        return Err(Win32Error::INVALID_DATA);
    }
    let mut copy = aligned_buffer(len);
    // SAFETY: Windows returned `len` bytes of descriptor at `owned`, which
    // stays allocated until the copy is done; `copy` has room for them and
    // does not overlap.
    unsafe {
        ptr::copy_nonoverlapping(owned.0.cast::<u8>(), copy.as_mut_ptr().cast::<u8>(), len);
    }
    Ok(copy)
}

fn manual_reset_event() -> Result<OwnedHandle, Win32Error> {
    // SAFETY: default security, manual reset, initially not signaled, no
    // name; there are no other inputs.
    let event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
    if event.is_null() {
        Err(Win32Error::last())
    } else {
        // SAFETY: CreateEventW returned a new handle that only this guard
        // owns and closes.
        Ok(unsafe { OwnedHandle::from_raw_handle(event) })
    }
}

/// What every instance of one pipe shares with its listener and stoppers.
#[derive(Debug)]
struct Shared {
    /// Instances of this pipe that this process has open. Changed only
    /// while holding the lock, together with creating or closing the
    /// instance, so it never counts one that is already closed.
    live: Mutex<u32>,
    /// Manual-reset event, signaled once to stop the listener and every
    /// pending connect, read and write.
    stop: OwnedHandle,
    stopped: AtomicBool,
}

/// Stops a [`PipeListener`] and all I/O on its instances, from any thread.
#[derive(Clone, Debug)]
pub struct PipeStopper(Arc<Shared>);

impl PipeStopper {
    /// A pending accept returns `None`; pending and later reads and writes
    /// fail with `ConnectionAborted`.
    pub fn stop(&self) {
        self.0.stopped.store(true, Ordering::SeqCst);
        // SAFETY: the event is live for as long as `Shared` is.
        unsafe {
            SetEvent(self.0.stop.as_raw_handle());
        }
    }

    pub fn is_stopped(&self) -> bool {
        self.0.stopped.load(Ordering::SeqCst)
    }
}

/// The server side of one local named pipe, in byte mode, refusing remote
/// clients (`PIPE_REJECT_REMOTE_CLIENTS`), with the security descriptor
/// given at [`bind`](Self::bind) and at most a fixed number of instances.
///
/// `bind` creates the first instance with `FILE_FLAG_FIRST_PIPE_INSTANCE`,
/// so it fails while any other process holds the name, and a pipe that
/// someone else created first is never joined. From then on the listener
/// keeps one instance waiting for the next client: each `accept` puts up
/// the next instance before it returns the connected one, so the name is
/// never without an instance of this listener while it is served. Should it
/// ever have none (the next instance could not be created and every
/// connection has closed), the next instance is created with the same flag
/// again. Other instances can only be added by a principal the descriptor
/// grants `FILE_CREATE_PIPE_INSTANCE`.
#[derive(Debug)]
pub struct PipeListener {
    name: Vec<u16>,
    security: Vec<u64>,
    max_instances: u32,
    shared: Arc<Shared>,
    /// The instance the next client connects to.
    waiting: Option<PipeInstance>,
}

impl PipeListener {
    pub fn bind(name: &str, sddl: &str, max_instances: u32) -> Result<Self, PipeError> {
        if !valid_pipe_name(name) {
            return Err(PipeError::InvalidName);
        }
        if !(1..=MAX_PIPE_INSTANCES).contains(&max_instances) {
            return Err(PipeError::InvalidLimit);
        }
        let security = security_descriptor(sddl).map_err(PipeError::InvalidSecurity)?;
        let shared = Arc::new(Shared {
            live: Mutex::new(0),
            stop: manual_reset_event().map_err(PipeError::Os)?,
            stopped: AtomicBool::new(false),
        });
        let mut listener = Self {
            name: wide(name),
            security,
            max_instances,
            shared,
            waiting: None,
        };
        listener.waiting = Some(listener.create_instance()?);
        Ok(listener)
    }

    pub fn stopper(&self) -> PipeStopper {
        PipeStopper(Arc::clone(&self.shared))
    }

    fn create_instance(&self) -> Result<PipeInstance, PipeError> {
        let event = manual_reset_event().map_err(PipeError::Os)?;
        let mut live = lock(&self.shared.live);
        let first = *live == 0;
        let open_mode = PIPE_ACCESS_DUPLEX
            | FILE_FLAG_OVERLAPPED
            | if first {
                FILE_FLAG_FIRST_PIPE_INSTANCE
            } else {
                0
            };
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_u32::<SECURITY_ATTRIBUTES>(),
            // Windows only reads the descriptor.
            lpSecurityDescriptor: self.security.as_ptr().cast_mut().cast(),
            bInheritHandle: 0,
        };
        // SAFETY: `name` is NUL-terminated UTF-16; `attributes` points to the
        // self-relative descriptor in `self.security`; both outlive the call,
        // and the other arguments are plain values.
        let handle = unsafe {
            CreateNamedPipeW(
                self.name.as_ptr(),
                open_mode,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                self.max_instances,
                PIPE_BUFFER_BYTES,
                PIPE_BUFFER_BYTES,
                0,
                &raw const attributes,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            let code = last_error();
            return Err(match code {
                ERROR_ACCESS_DENIED if first => PipeError::NameInUse,
                // The descriptor names an owner or group this account may
                // not assign, or needs a privilege it does not hold.
                ERROR_INVALID_OWNER | ERROR_INVALID_PRIMARY_GROUP | ERROR_PRIVILEGE_NOT_HELD => {
                    PipeError::InvalidSecurity(Win32Error { code })
                }
                _ => PipeError::Os(Win32Error { code }),
            });
        }
        *live += 1;
        Ok(PipeInstance {
            // SAFETY: CreateNamedPipeW returned a new, valid handle that only
            // this instance owns and closes.
            pipe: Some(unsafe { OwnedHandle::from_raw_handle(handle) }),
            event,
            shared: Arc::clone(&self.shared),
        })
    }

    /// Waits for the next client and returns its connected instance, or
    /// `None` once the listener is stopped. A client that connects and
    /// closes before it is served is skipped.
    pub fn accept(&mut self) -> Result<Option<PipeInstance>, PipeError> {
        let mut failures = 0;
        loop {
            if self.shared.stopped.load(Ordering::SeqCst) {
                return Ok(None);
            }
            let mut instance = match self.waiting.take() {
                Some(instance) => instance,
                None => self.create_instance()?,
            };
            match instance.connect() {
                Ok(true) => {
                    // The next instance goes up before this one is handed
                    // out. If that fails, the connected instance still holds
                    // the name and the next accept tries again.
                    self.waiting = self.create_instance().ok();
                    return Ok(Some(instance));
                }
                Ok(false) => return Ok(None),
                // The client closed before it was served; use a fresh
                // instance.
                Err(ERROR_NO_DATA) => {}
                Err(code) => {
                    failures += 1;
                    if failures >= MAX_CONNECT_FAILURES {
                        return Err(PipeError::Os(Win32Error { code }));
                    }
                }
            }
        }
    }
}

/// Why an overlapped operation did not complete on its own.
#[derive(Debug)]
enum Ended {
    Stopped,
    TimedOut,
    Failed(u32),
}

/// Milliseconds to wait for `deadline`, rounded up so a wait never ends
/// just short of it; `INFINITE` without one.
fn wait_millis(deadline: Option<Instant>) -> u32 {
    deadline.map_or(INFINITE, |deadline| {
        let left = deadline.saturating_duration_since(Instant::now());
        u32::try_from(left.as_nanos().div_ceil(1_000_000))
            .unwrap_or(u32::MAX)
            .min(INFINITE - 1)
    })
}

fn os_error(code: u32) -> io::Error {
    io::Error::from_raw_os_error(i32::try_from(code).unwrap_or(i32::MAX))
}

fn io_ended(ended: Ended) -> io::Error {
    match ended {
        Ended::Stopped => io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "the pipe server is stopping",
        ),
        Ended::TimedOut => io::Error::new(
            io::ErrorKind::TimedOut,
            "the pipe operation missed its deadline",
        ),
        Ended::Failed(code) => os_error(code),
    }
}

/// The client has closed its end.
fn client_gone(code: u32) -> bool {
    matches!(
        code,
        ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED | ERROR_NO_DATA
    )
}

/// One instance of a pipe, connected to a client once
/// [`PipeListener::accept`] returns it, and closed on drop (the client then
/// sees the end of the stream).
///
/// Reads and writes are overlapped and end at their deadline or when the
/// listener is stopped: the operation is cancelled with `CancelIoEx`, and
/// the call still waits until Windows reports it complete before it returns,
/// so neither its `OVERLAPPED` nor its buffer is ever released while Windows
/// may write to it.
#[derive(Debug)]
pub struct PipeInstance {
    /// `None` only while dropping.
    pipe: Option<OwnedHandle>,
    /// Signaled when this instance's current operation completes.
    event: OwnedHandle,
    shared: Arc<Shared>,
}

impl Drop for PipeInstance {
    fn drop(&mut self) {
        // Closed while holding the count's lock: see `Shared::live`.
        let mut live = lock(&self.shared.live);
        drop(self.pipe.take());
        *live = live.saturating_sub(1);
    }
}

impl PipeInstance {
    fn raw(&self) -> HANDLE {
        self.pipe
            .as_ref()
            .map_or(INVALID_HANDLE_VALUE, AsRawHandle::as_raw_handle)
    }

    fn overlapped(&self) -> OVERLAPPED {
        OVERLAPPED {
            hEvent: self.event.as_raw_handle(),
            ..OVERLAPPED::default()
        }
    }

    /// Waits for the operation started with `overlapped` until it completes,
    /// `deadline` passes or the listener is stopped. In the last two cases it
    /// is cancelled, and this still waits for Windows to report it complete:
    /// only after that may `overlapped` and the operation's buffer go away.
    /// An operation that completed just before the cancel keeps its result.
    fn finish(&self, overlapped: &mut OVERLAPPED, deadline: Option<Instant>) -> Result<u32, Ended> {
        let events = [self.event.as_raw_handle(), self.shared.stop.as_raw_handle()];
        let interrupted = loop {
            // SAFETY: both are live event handles owned by `self`, and the
            // count matches the array.
            let waited =
                unsafe { WaitForMultipleObjects(2, events.as_ptr(), 0, wait_millis(deadline)) };
            match waited {
                WAIT_OBJECT_0 => break None,
                stop if stop == WAIT_OBJECT_0 + 1 => break Some(Ended::Stopped),
                // A timed wait can end up to a timer tick early; the
                // operation gets until the deadline itself.
                WAIT_TIMEOUT if deadline.is_some_and(|d| Instant::now() < d) => {}
                WAIT_TIMEOUT => break Some(Ended::TimedOut),
                _ => break Some(Ended::Failed(last_error())),
            }
        };
        if interrupted.is_some() {
            // SAFETY: cancels only the operation started with `overlapped`,
            // which is still in place; one that already completed is left
            // as it is.
            unsafe {
                CancelIoEx(self.raw(), &raw const *overlapped);
            }
        }
        let mut transferred = 0_u32;
        // SAFETY: with bWait set this returns only once the operation on
        // `overlapped` has completed, cancelled or not, so Windows is done
        // with it and with the buffer when the caller continues.
        let ok = unsafe {
            GetOverlappedResult(self.raw(), &raw const *overlapped, &raw mut transferred, 1)
        };
        if ok != 0 {
            return Ok(transferred);
        }
        let code = last_error();
        match interrupted {
            Some(reason @ (Ended::Stopped | Ended::TimedOut))
                if code == ERROR_OPERATION_ABORTED =>
            {
                Err(reason)
            }
            _ => Err(Ended::Failed(code)),
        }
    }

    /// Waits for a client: `Ok(true)` once one is connected, `Ok(false)` if
    /// the listener was stopped first.
    fn connect(&mut self) -> Result<bool, u32> {
        let mut overlapped = self.overlapped();
        // SAFETY: the pipe handle is live and not yet connected; `overlapped`
        // carries this instance's event and stays in place until `finish`
        // has seen the operation complete.
        let ok = unsafe { ConnectNamedPipe(self.raw(), &raw mut overlapped) };
        if ok == 0 {
            match last_error() {
                // A client opened this instance before the call.
                ERROR_PIPE_CONNECTED => return Ok(true),
                ERROR_IO_PENDING => {}
                code => return Err(code),
            }
        }
        match self.finish(&mut overlapped, None) {
            Ok(_) => Ok(true),
            Err(Ended::Stopped | Ended::TimedOut) => Ok(false),
            Err(Ended::Failed(code)) => Err(code),
        }
    }

    /// The connected client's identity. The thread impersonates the client
    /// (`ImpersonateNamedPipeClient`) only long enough to open its token
    /// (`OpenThreadToken`, checked against this process's own rights,
    /// because an identification-level token cannot open objects) and then
    /// always reverts (`RevertToSelf`); a thread that cannot revert stops the
    /// process. The token is read after reverting. Clients connect at
    /// `SecurityIdentification`, which is enough to read it.
    pub fn client_identity(&self) -> Result<TokenIdentity, Win32Error> {
        let token = {
            // SAFETY: the pipe handle is live and connected.
            if unsafe { ImpersonateNamedPipeClient(self.raw()) } == 0 {
                return Err(Win32Error::last());
            }
            let _revert = Impersonating;
            let mut token = ptr::null_mut();
            // SAFETY: GetCurrentThread returns a pseudo-handle that needs no
            // closing; `token` is a valid out pointer.
            let ok = unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &raw mut token) };
            if ok == 0 {
                // Read before `_revert` runs RevertToSelf.
                let error = Win32Error::last();
                return Err(error);
            }
            // SAFETY: OpenThreadToken returned a new handle that only this
            // guard owns and closes.
            unsafe { OwnedHandle::from_raw_handle(token) }
        };
        token_identity(&token)
    }

    /// Reads what the client has sent, at most `buf.len()` bytes; `Ok(0)`
    /// once the client has closed its end. Fails with `TimedOut` at
    /// `deadline` and `ConnectionAborted` once the listener is stopped.
    pub fn read(&mut self, buf: &mut [u8], deadline: Instant) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        let mut overlapped = self.overlapped();
        // SAFETY: `buf` is writable for `len` bytes and stays borrowed, like
        // `overlapped` stays in place, until `finish` has seen the read
        // complete; the byte count comes from `finish`, so its pointer is
        // null as overlapped reads require.
        let ok = unsafe {
            ReadFile(
                self.raw(),
                buf.as_mut_ptr(),
                len,
                ptr::null_mut(),
                &raw mut overlapped,
            )
        };
        if ok == 0 {
            let code = last_error();
            if code != ERROR_IO_PENDING {
                return if client_gone(code) {
                    Ok(0)
                } else {
                    Err(os_error(code))
                };
            }
        }
        match self.finish(&mut overlapped, Some(deadline)) {
            Ok(read) => Ok(read as usize),
            Err(Ended::Failed(code)) if client_gone(code) => Ok(0),
            Err(ended) => Err(io_ended(ended)),
        }
    }

    /// Writes up to `buf.len()` bytes, with the same deadline and stop rules
    /// as [`read`](Self::read). Fails with `BrokenPipe` once the client has
    /// closed its end.
    pub fn write(&mut self, buf: &[u8], deadline: Instant) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        let mut overlapped = self.overlapped();
        // SAFETY: as for `read`, with `buf` readable for `len` bytes.
        let ok = unsafe {
            WriteFile(
                self.raw(),
                buf.as_ptr(),
                len,
                ptr::null_mut(),
                &raw mut overlapped,
            )
        };
        if ok == 0 {
            let code = last_error();
            if code != ERROR_IO_PENDING {
                return Err(os_error(code));
            }
        }
        match self.finish(&mut overlapped, Some(deadline)) {
            Ok(written) => Ok(written as usize),
            Err(ended) => Err(io_ended(ended)),
        }
    }
}

/// Who owns a file, folder or pipe, and whether anyone else may change it,
/// from its security descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectSecurity {
    /// The owner as a SID string.
    pub owner: String,
    /// The owner is SYSTEM or Administrators.
    pub owner_privileged: bool,
    /// There is no DACL (everyone has full access), or it allows a principal
    /// other than SYSTEM and Administrators a right in [`CHANGE_RIGHTS`].
    /// Deny entries and entries that only apply to children do not count;
    /// an allow entry of any type other than the plain one counts whoever it
    /// names.
    pub others_may_change: bool,
    /// The owner and DACL in SDDL, as evidence for an operator.
    pub sddl: String,
}

/// Rights that change a file or folder, or what a folder holds: write or
/// append data (for a folder: add a file or a subfolder), write extended
/// attributes or attributes, delete a child, delete, `WRITE_DAC`,
/// `WRITE_OWNER`, `GENERIC_WRITE` and `GENERIC_ALL`.
pub const CHANGE_RIGHTS: u32 = FILE_WRITE_DATA
    | FILE_APPEND_DATA
    | FILE_WRITE_EA
    | FILE_DELETE_CHILD
    | FILE_WRITE_ATTRIBUTES
    | DELETE
    | WRITE_DAC
    | WRITE_OWNER
    | GENERIC_WRITE
    | GENERIC_ALL;

/// ACE types from `winnt.h`; the enabled windows-sys features do not carry
/// them.
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
/// `ACCESS_DENIED_ACE_TYPE` and its object, callback and callback-object
/// forms: they only take rights away.
const DENY_ACE_TYPES: [u8; 4] = [1, 6, 10, 12];
/// Bytes of every ACE before its SID: the header and the access mask.
const ACE_PREFIX_BYTES: usize = 8;

/// Whether the DACL at `dacl`, inside a descriptor Windows returned and the
/// caller keeps allocated, lets anyone but SYSTEM and Administrators change
/// the object.
fn dacl_lets_others_change(dacl: *const ACL) -> Result<bool, Win32Error> {
    // SAFETY: `dacl` points to an ACL inside a descriptor Windows built; the
    // header is copied out unaligned.
    let header = unsafe { dacl.read_unaligned() };
    let start = dacl as usize;
    let end = start + usize::from(header.AclSize);
    for index in 0..u32::from(header.AceCount) {
        let mut ace = ptr::null_mut();
        // SAFETY: `index` is below the ACL's own entry count; `ace` is a
        // valid out pointer.
        if unsafe { GetAce(dacl, index, &raw mut ace) } == 0 {
            return Err(Win32Error::last());
        }
        let at = ace as usize;
        if at < start || at.checked_add(ACE_PREFIX_BYTES).is_none_or(|e| e > end) {
            return Err(Win32Error::INVALID_DATA);
        }
        // SAFETY: the entry's first bytes lie inside the ACL (checked above);
        // copied out unaligned.
        let entry = unsafe { ace.cast::<ACE_HEADER>().read_unaligned() };
        let size = usize::from(entry.AceSize);
        if size < ACE_PREFIX_BYTES || at + size > end {
            return Err(Win32Error::INVALID_DATA);
        }
        if u32::from(entry.AceFlags) & INHERIT_ONLY_ACE != 0
            || DENY_ACE_TYPES.contains(&entry.AceType)
        {
            continue;
        }
        // SAFETY: every ACE type that grants access has its access mask
        // right after the 4-byte header, inside the entry (at least 8 bytes).
        let mask = unsafe { ace.cast::<u8>().add(4).cast::<u32>().read_unaligned() };
        if mask & CHANGE_RIGHTS == 0 {
            continue;
        }
        let sid: PSID = ace.cast::<u8>().wrapping_add(ACE_PREFIX_BYTES).cast();
        let privileged = entry.AceType == ACCESS_ALLOWED_ACE_TYPE
            && sid_within(sid, at, at + size)
            && is_privileged_sid(sid);
        if !privileged {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Reads the owner and DACL of an open file, folder or pipe. The handle
/// needs `READ_CONTROL`, which [`open_folder_no_follow`],
/// [`open_file_no_follow`] and [`PIPE_CLIENT_ACCESS`] include.
pub fn object_security(file: &File) -> Result<ObjectSecurity, Win32Error> {
    let what = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
    let mut owner: PSID = ptr::null_mut();
    let mut dacl: *mut ACL = ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: the handle is live for the borrow of `file`; the out pointers
    // are valid locals; the owner and DACL pointers point into `descriptor`,
    // which Windows allocates for the caller and `_descriptor` frees.
    let code = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            what,
            &raw mut owner,
            ptr::null_mut(),
            &raw mut dacl,
            ptr::null_mut(),
            &raw mut descriptor,
        )
    };
    let _descriptor = LocalMemory(descriptor);
    if code != NO_ERROR {
        return Err(Win32Error { code });
    }
    // SAFETY: a non-null owner points into the descriptor, still allocated.
    if owner.is_null() || unsafe { IsValidSid(owner) } == 0 {
        return Err(Win32Error::INVALID_DATA);
    }
    let owner_text = sid_text(owner)?;
    let others_may_change = dacl.is_null() || dacl_lets_others_change(dacl)?;
    let mut text = ptr::null_mut();
    // SAFETY: `descriptor` is the live descriptor from above; `text` is a
    // valid out pointer and the length pointer may be null.
    let ok = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SDDL_REVISION_1,
            what,
            &raw mut text,
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(Win32Error::last());
    }
    let sddl = ReturnedText {
        text,
        co_task: false,
    }
    .string()?;
    Ok(ObjectSecurity {
        owner: owner_text,
        owner_privileged: is_privileged_sid(owner),
        others_may_change,
        sddl,
    })
}

/// Opens a folder itself, never what a symlink or junction there points to,
/// to list it and read its attributes and security. While it is open, nobody
/// can rename or delete it: it is not shared for delete, and the listing
/// access is what makes Windows apply sharing at all (a handle with only
/// attribute or security access takes no part in it).
pub fn open_folder_no_follow(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .access_mode(FILE_GENERIC_READ)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

/// Opens a file itself for reading, never what a symlink there points to.
/// While it is open, nobody can write, rename or delete it (read sharing
/// only).
pub fn open_file_no_follow(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

/// How many names (hard links) the open file has. A file with more than one
/// is changed under every name when it is written in place.
pub fn link_count(file: &File) -> io::Result<u32> {
    // SAFETY: an all-zero BY_HANDLE_FILE_INFORMATION is a valid value of
    // this plain-data struct.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: the handle is live for the borrow of `file`, and `info` is a
    // valid local Windows fills in.
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &raw mut info) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(info.nNumberOfLinks)
}

/// Whether an open file or folder is itself a reparse point: a symlink,
/// junction, mount point or anything else Windows would redirect.
pub fn is_reparse_point(file: &File) -> io::Result<bool> {
    Ok(file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
}

/// Opens a file to append to it, creating it if missing, never following a
/// symlink there, and refuses one that is a reparse point or not a plain
/// file. While it is open, others may read it but not write, rename or
/// delete it.
pub fn open_append_no_follow(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    if is_reparse_point(&file)? || !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the file is a link or not a plain file",
        ));
    }
    Ok(file)
}

/// The longest path Windows has, in UTF-16 units with the NUL.
const MAX_PATH_UNITS: usize = 32_768;
/// The first buffer [`final_path_of_file`] tries, in UTF-16 units: `MAX_PATH`.
const FIRST_PATH_UNITS: usize = 260;

/// Where an open file or folder really is, from its handle
/// (`GetFinalPathNameByHandleW`, normalized, with the DOS volume name), the
/// form `std::fs::canonicalize` gives: `\\?\C:\…`, or `\\?\UNC\…` on a
/// share. Bound to the handle rather than a path, so a link, junction or
/// folder that is swapped or moved after the file was opened cannot make it
/// name another file. The buffer grows only to what Windows says it needs,
/// at most [`MAX_PATH_UNITS`], and a path that keeps growing between two
/// reads fails with `InvalidData` rather than being read again and again.
pub fn final_path_of_file(file: &File) -> io::Result<PathBuf> {
    let mut units = FIRST_PATH_UNITS;
    // The second read gets the size the first asked for; one more covers a
    // rename in between.
    for _ in 0..3 {
        let mut buffer = vec![0_u16; units];
        let capacity = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
        // SAFETY: the handle is live for the borrow of `file`; `buffer` holds
        // `capacity` UTF-16 units, and Windows writes at most that many.
        let written = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                buffer.as_mut_ptr(),
                capacity,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        };
        let written = usize::try_from(written).unwrap_or(usize::MAX);
        if written == 0 {
            return Err(io::Error::last_os_error());
        }
        if written < buffer.len() {
            // The path without its NUL.
            buffer.truncate(written);
            return Ok(PathBuf::from(OsString::from_wide(&buffer)));
        }
        // Too small: `written` is the size needed, with the NUL.
        if written > MAX_PATH_UNITS {
            break;
        }
        units = written;
    }
    Err(io::Error::from(io::ErrorKind::InvalidData))
}

/// LocalSystem: the account the WGAL service runs as, and the only owner the
/// watchdog's pipe may have (its descriptor names SYSTEM as owner, which only
/// SYSTEM can assign).
pub const LOCAL_SYSTEM_SID: &str = "S-1-5-18";

/// The pipe under the watchdog's name was created by another account, not
/// by the WGAL service: the error inside what [`connect_watchdog_pipe`]
/// returns then. Recognize it with [`is_not_the_watchdog`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotTheWatchdog;

impl fmt::Display for NotTheWatchdog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the pipe is not served by the WGAL service")
    }
}

impl std::error::Error for NotTheWatchdog {}

/// Whether `error` came from a pipe that is not the WGAL service's.
pub fn is_not_the_watchdog(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|inner| inner.is::<NotTheWatchdog>())
}

/// Opens the watchdog's pipe ([`wgal_protocol::WINDOWS_PIPE_NAME`]) the way
/// every WGAL client must: with exactly [`PIPE_CLIENT_ACCESS`], at
/// identification level ([`PIPE_CLIENT_QOS`]), and only if LocalSystem owns
/// it. Anything another account created under that name, while the service
/// is not running, fails with `ConnectionRefused` carrying
/// [`NotTheWatchdog`], before a single byte is written, so a squatter never
/// sees a request (or a bot token). Other failures are the OS's own errors,
/// for example `NotFound` when nothing serves the pipe and
/// `ERROR_PIPE_BUSY` (231) while every instance is in use.
pub fn connect_watchdog_pipe() -> io::Result<File> {
    open_pipe_owned_by(wgal_protocol::WINDOWS_PIPE_NAME, LOCAL_SYSTEM_SID)
}

/// [`connect_watchdog_pipe`] for any pipe and expected owner. Private: the
/// only name and owner outside tests are the watchdog's.
fn open_pipe_owned_by(name: &str, owner: &str) -> io::Result<File> {
    let pipe = OpenOptions::new()
        .access_mode(PIPE_CLIENT_ACCESS)
        .security_qos_flags(PIPE_CLIENT_QOS)
        .open(name)?;
    let security = object_security(&pipe).map_err(|e| os_error(e.code))?;
    if security.owner == owner {
        Ok(pipe)
    } else {
        // Dropping `pipe` closes it unwritten.
        Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            NotTheWatchdog,
        ))
    }
}

/// The ProgramData folder (`FOLDERID_ProgramData`), from the shell, never
/// from an environment variable.
pub fn program_data_dir() -> Result<PathBuf, Win32Error> {
    let mut path = ptr::null_mut();
    // SAFETY: FOLDERID_ProgramData is a static GUID; default flags and no
    // token (the folder is the same for every account); `path` is a valid
    // out pointer.
    let result =
        unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramData, 0, ptr::null_mut(), &raw mut path) };
    // Freed whether or not the call succeeded, as the API requires.
    let path = ReturnedText {
        text: path,
        co_task: true,
    };
    if result != S_OK {
        return Err(Win32Error {
            code: result.cast_unsigned(),
        });
    }
    let path = PathBuf::from(OsString::from_wide(
        path.units().ok_or(Win32Error::INVALID_DATA)?,
    ));
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(Win32Error::INVALID_DATA)
    }
}

// --- The console's echo, while a secret is typed (owner decision of
// 2026-11-18: `wgal notify token set` asks for the Bot Token unseen).

/// `wincon.h`: `ENABLE_ECHO_INPUT`, the console showing what is typed.
const CONSOLE_ECHO_INPUT: u32 = ENABLE_ECHO_INPUT;

/// A console input mode with the echo off and everything else as it was.
pub fn console_mode_without_echo(mode: u32) -> u32 {
    mode & !CONSOLE_ECHO_INPUT
}

/// This process's standard input handle, or `None` when it has none. The
/// handle is the process's own and never closed here.
fn stdin_handle() -> Option<HANDLE> {
    // SAFETY: GetStdHandle takes a constant and only reads the process's
    // standard handle table.
    let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    (!handle.is_null() && handle != INVALID_HANDLE_VALUE).then_some(handle)
}

/// The console input mode of standard input, or `None` when standard input
/// is not a console (a file, a pipe, or none).
fn stdin_console_mode() -> Option<u32> {
    let handle = stdin_handle()?;
    let mut mode = 0;
    // SAFETY: `handle` is the process's standard input handle; `mode` is a
    // valid out pointer for one `u32`. GetConsoleMode fails for a handle
    // that is not a console's.
    (unsafe { GetConsoleMode(handle, &raw mut mode) } != 0).then_some(mode)
}

/// Sets standard input's console input mode.
fn set_stdin_console_mode(mode: u32) -> Result<(), Win32Error> {
    let handle = stdin_handle().ok_or(Win32Error::INVALID_PARAMETER)?;
    // SAFETY: `handle` is the process's standard input handle and `mode` a
    // plain value; SetConsoleMode fails for a handle that is not a
    // console's.
    if unsafe { SetConsoleMode(handle, mode) } == 0 {
        return Err(Win32Error::last());
    }
    Ok(())
}

/// Adds [`on_interrupt`] to the process's console control handlers, or
/// removes it.
fn interrupt_handler(add: bool) -> Result<(), Win32Error> {
    // SAFETY: `on_interrupt` is a plain function that lives as long as the
    // process; TRUE adds it to the handler list, FALSE removes it.
    if unsafe { SetConsoleCtrlHandler(Some(on_interrupt), i32::from(add)) } == 0 {
        return Err(Win32Error::last());
    }
    Ok(())
}

/// The console input mode to put back while the echo is off. Changed only
/// under its lock, which the interrupt handler takes too, so the handler
/// never puts the mode back before the echo is off (it waits for the
/// change under way), and never after it was put back already (Codex
/// review of the CLI, P2-6).
static ECHO_OFF: Mutex<Option<u32>> = Mutex::new(None);

fn echo_off() -> std::sync::MutexGuard<'static, Option<u32>> {
    ECHO_OFF
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Turns the echo of standard input's console off, and adds the handler
/// that, on an interrupt (Ctrl+C, Ctrl+Break, closing the console, logging
/// off, shutting down), puts the mode back first and then leaves the
/// interrupt to the next handler, in the end the default one, which ends
/// the process. Fails, changing nothing, when standard input is not a
/// console or the echo is off already.
pub fn hide_stdin_echo() -> Result<(), Win32Error> {
    let mut saved = echo_off();
    if saved.is_some() {
        return Err(Win32Error::INVALID_PARAMETER);
    }
    let mode = stdin_console_mode().ok_or(Win32Error::INVALID_PARAMETER)?;
    interrupt_handler(true)?;
    if let Err(error) = set_stdin_console_mode(console_mode_without_echo(mode)) {
        let _ = interrupt_handler(false);
        return Err(error);
    }
    *saved = Some(mode);
    Ok(())
}

/// Puts standard input's console mode back as [`hide_stdin_echo`] found it
/// and removes the interrupt handler; nothing when the echo is not off.
pub fn restore_stdin_echo() {
    let mut saved = echo_off();
    if let Some(mode) = saved.take() {
        let _ = set_stdin_console_mode(mode);
        let _ = interrupt_handler(false);
    }
}

/// The console control handler: puts the saved mode back, if any, and
/// answers FALSE so the next handler, in the end the default one, deals
/// with the interrupt. Windows calls it on a thread of its own.
extern "system" fn on_interrupt(_control: u32) -> windows_sys::core::BOOL {
    if let Some(mode) = echo_off().take() {
        let _ = set_stdin_console_mode(mode);
    }
    0
}

/// Read-only smoke tests of the wrappers against things that cannot be a
/// VPN: the EventLog service and the loopback interface. They never open a
/// WireGuard service, never send to anything but 127.0.0.1, and print
/// nothing about this host.
#[cfg(test)]
mod tests {
    use super::*;

    const LOOPBACK: Ipv4Addr = Ipv4Addr::LOCALHOST;

    #[test]
    fn counts_a_files_names() {
        let dir = std::env::temp_dir().join(format!("wgal-links-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(&path, b"{}").unwrap();
        assert_eq!(link_count(&File::open(&path).unwrap()).unwrap(), 1);
        std::fs::hard_link(&path, dir.join("second")).unwrap();
        assert_eq!(link_count(&File::open(&path).unwrap()).unwrap(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn queries_a_builtin_service_read_only() {
        let state = query_service("EventLog");
        assert_eq!(state, Ok(ServiceState::Running));
    }

    #[test]
    fn a_missing_service_reports_its_win32_error() {
        let state = query_service("WgalSmokeTestNoSuchService");
        assert_eq!(state, Err(ScmError::SERVICE_DOES_NOT_EXIST));
        assert_eq!(query_service("Event\0Log"), Err(ScmError::INVALID_NAME));
    }

    #[test]
    fn lists_service_names() {
        let names = service_names().map_err(|e| e.code);
        let has_eventlog = names
            .as_ref()
            .is_ok_and(|n| n.iter().any(|s| s.eq_ignore_ascii_case("EventLog")));
        assert!(
            has_eventlog,
            "service enumeration failed or lacked EventLog"
        );
    }

    #[test]
    fn finds_the_loopback_adapter() {
        // IF_TYPE_SOFTWARE_LOOPBACK; checks that the type is read correctly.
        let found = adapters().is_ok_and(|list| {
            list.iter()
                .any(|a| a.ipv4.contains(&LOOPBACK) && a.if_type == 24 && a.up)
        });
        assert!(
            found,
            "adapter enumeration failed or lacked the loopback adapter"
        );
    }

    /// The listing for the offline check holds the same adapters, and the
    /// IPv4 listing never carries an IPv6 address. Prints nothing.
    #[test]
    fn lists_adapters_with_both_families() {
        let v4 = adapters().map_err(|e| e.code);
        let any = adapters_any_family().map_err(|e| e.code);
        assert!(
            v4.as_ref()
                .is_ok_and(|list| list.iter().all(|a| a.ipv6.is_empty())),
            "the IPv4 listing failed or carried IPv6 addresses"
        );
        let has_loopback = any.as_ref().is_ok_and(|list| {
            list.iter()
                .any(|a| a.ipv4.contains(&LOOPBACK) && a.if_type == 24 && a.up)
        });
        assert!(
            has_loopback,
            "the listing of both families failed or lacked the loopback adapter"
        );
    }

    #[test]
    fn routes_loopback_from_loopback() {
        let route = best_route(LOOPBACK).map(|r| r.source);
        assert_eq!(route, Ok(LOOPBACK));
    }

    #[test]
    fn a_protected_secret_round_trips_and_is_not_plain() {
        let secret = b"123456789:EXAMPLE-not-a-real-token_0000000000";
        let sealed = protect_secret(secret).unwrap();
        assert!(!sealed.windows(secret.len()).any(|w| w == secret));
        assert_eq!(unprotect_secret(&sealed).unwrap(), secret);
        let mut tampered = sealed.clone();
        if let Some(last) = tampered.last_mut() {
            *last ^= 0xff;
        }
        assert!(unprotect_secret(&tampered).is_err());
        assert!(unprotect_secret(b"not a dpapi blob").is_err());
        assert!(protect_secret(&vec![0; MAX_SECRET_BYTES + 1]).is_err());
    }

    #[test]
    fn this_process_has_a_canonical_identity() {
        let me = current_identity().unwrap();
        assert!(is_canonical_sid(&me.sid));
    }

    #[test]
    fn only_canonical_sids_are_accepted() {
        for ok in [
            "S-1-5-18",
            "S-1-5-32-544",
            "S-1-1-0",
            "S-1-5-21-1000000000-2000000000-3000000000-1001",
            "S-1-12-1-1000000000-2000000000-3000000000-4000000000",
        ] {
            assert!(is_canonical_sid(ok), "{ok}");
        }
        let sixteen = format!("S-1-5{}", "-1".repeat(16));
        for bad in [
            "",
            "S-1-",
            "S-1-5-",
            "s-1-5-18",
            "BA",
            "SY",
            "S-1-5-018",
            "S-1-0x5-18",
            "S-1-5--18",
            "S-1-5-18-",
            " S-1-5-18",
            "S-1-5-18 ",
            "S-1-5-18\0",
            "S-2-5-18",
            "S-1-5-21-1-2-3-4)(A;;GA;;;WD)",
            "S-1-5-21-4294967296-1-2-3",
            sixteen.as_str(),
        ] {
            assert!(!is_canonical_sid(bad), "{bad:?}");
        }
    }

    #[test]
    fn program_data_is_an_absolute_folder() {
        let dir = program_data_dir().unwrap();
        assert!(dir.is_absolute() && dir.is_dir());
    }

    #[test]
    fn only_valid_sddl_becomes_a_descriptor() {
        assert!(security_descriptor("D:P(A;;GA;;;SY)").is_ok());
        for bad in [
            "",
            "not sddl",
            "D:P(A;;GA;;;SY)\0",
            "D:P(A;;GA;;;S-1-5-21-x)",
        ] {
            assert!(security_descriptor(bad).is_err(), "{bad:?}");
        }
    }

    fn others_may_change(sddl: &str) -> bool {
        use windows_sys::Win32::Security::GetSecurityDescriptorDacl;
        let descriptor = security_descriptor(sddl).unwrap();
        let (mut present, mut defaulted) = (0, 0);
        let mut dacl = ptr::null_mut();
        // SAFETY: `descriptor` holds a valid self-relative descriptor that
        // outlives the call and the use of `dacl`, which points into it; the
        // out pointers are valid locals.
        let ok = unsafe {
            GetSecurityDescriptorDacl(
                descriptor.as_ptr().cast_mut().cast(),
                &raw mut present,
                &raw mut dacl,
                &raw mut defaulted,
            )
        };
        assert!(ok != 0 && present != 0 && !dacl.is_null());
        dacl_lets_others_change(dacl).unwrap()
    }

    #[test]
    fn only_system_and_administrators_may_change_an_admin_only_dacl() {
        assert!(!others_may_change(
            "D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FR;;;BU)"
        ));
        // Deny entries and entries only for children grant nothing here.
        assert!(!others_may_change(
            "D:P(D;;FA;;;WD)(A;;FA;;;SY)(A;OICIIO;FA;;;CO)(A;;0x1200a9;;;AU)"
        ));
        for writable in [
            "D:P(A;;FA;;;SY)(A;;FW;;;BU)",
            "D:P(A;;FA;;;SY)(A;;0x2;;;S-1-5-21-1000000000-2000000000-3000000000-1001)",
            "D:P(A;;FA;;;SY)(A;;0x4;;;AU)",
            "D:P(A;;FA;;;SY)(A;;0x10;;;AU)",
            "D:P(A;;FA;;;SY)(A;;0x40;;;AU)",
            "D:P(A;;FA;;;SY)(A;;0x100;;;AU)",
            "D:P(A;;FA;;;SY)(A;;SD;;;WD)",
            "D:P(A;;FA;;;SY)(A;;WD;;;BU)",
            "D:P(A;;FA;;;SY)(A;;WO;;;BU)",
            "D:P(A;;FA;;;SY)(A;;GA;;;BU)",
            "D:P(A;;FA;;;SY)(A;;GW;;;BU)",
            // What a new folder under ProgramData inherits for Users.
            "D:P(A;;FA;;;SY)(A;CI;0x116;;;BU)",
            "D:P(A;;FA;;;SY)(A;;FA;;;OW)",
        ] {
            assert!(others_may_change(writable), "{writable}");
        }
    }

    /// A fresh directory under the temp folder, removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("wgal-sys-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_file_the_user_created_is_not_admin_only() {
        let scratch = Scratch::new("security");
        let path = scratch.0.join("file.txt");
        std::fs::write(&path, b"x").unwrap();
        let security = object_security(&open_file_no_follow(&path).unwrap()).unwrap();
        assert!(security.others_may_change);
        assert!(is_canonical_sid(&security.owner));
        assert!(security.sddl.starts_with("O:"));
        let folder = object_security(&open_folder_no_follow(&scratch.0).unwrap()).unwrap();
        assert!(folder.others_may_change);
    }

    #[test]
    fn what_is_open_can_be_neither_renamed_nor_rewritten() {
        let scratch = Scratch::new("hold");
        let folder = scratch.0.join("folder");
        std::fs::create_dir(&folder).unwrap();
        let file = folder.join("file.txt");
        std::fs::write(&file, b"x").unwrap();

        let held = open_file_no_follow(&file).unwrap();
        assert!(held.metadata().unwrap().is_file());
        assert!(!is_reparse_point(&held).unwrap());
        assert!(std::fs::write(&file, b"y").is_err());
        assert!(std::fs::rename(&file, folder.join("moved.txt")).is_err());
        drop(held);

        let held = open_folder_no_follow(&folder).unwrap();
        assert!(held.metadata().unwrap().is_dir());
        assert!(!is_reparse_point(&held).unwrap());
        assert!(std::fs::rename(&folder, scratch.0.join("moved")).is_err());
        drop(held);
        std::fs::rename(&folder, scratch.0.join("moved")).unwrap();
    }

    /// Creating symlinks needs Developer Mode or the symlink privilege; the
    /// check is skipped where neither is available.
    #[test]
    fn symlinks_are_opened_themselves_and_seen_as_reparse_points() {
        use std::os::windows::fs::{symlink_dir, symlink_file};
        let scratch = Scratch::new("links");
        let target = scratch.0.join("target.txt");
        std::fs::write(&target, b"x").unwrap();
        let link = scratch.0.join("link.txt");
        if symlink_file(&target, &link).is_ok() {
            assert!(is_reparse_point(&open_file_no_follow(&link).unwrap()).unwrap());
        }
        let folder_link = scratch.0.join("folder-link");
        if symlink_dir(&scratch.0, &folder_link).is_ok() {
            assert!(is_reparse_point(&open_folder_no_follow(&folder_link).unwrap()).unwrap());
        }
    }

    /// Turns the empty folder `link` into a junction to `target`, as
    /// `mklink /J` does; any account may do this to a folder it owns.
    fn make_junction(link: &Path, target: &Path) {
        use windows_sys::Win32::System::IO::DeviceIoControl;
        // winioctl.h and winnt.h; not in the enabled windows-sys features.
        const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00a4;
        const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xa000_0003;

        let target = target.to_str().unwrap();
        let substitute: Vec<u16> = format!(r"\??\{target}").encode_utf16().collect();
        let print: Vec<u16> = target.encode_utf16().collect();
        let units = |v: &Vec<u16>| u16::try_from(v.len() * 2).unwrap();
        // Tag, data length, reserved; then the mount point header: offsets
        // and lengths of both names, which follow with a NUL each.
        let mut data: Vec<u16> = Vec::new();
        data.extend([0, units(&substitute), units(&substitute) + 2, units(&print)]);
        data.extend(&substitute);
        data.push(0);
        data.extend(&print);
        data.push(0);
        let mut buffer = Vec::new();
        buffer.extend(IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
        buffer.extend(u16::try_from(data.len() * 2).unwrap().to_le_bytes());
        buffer.extend(0_u16.to_le_bytes());
        for unit in data {
            buffer.extend(unit.to_le_bytes());
        }
        let folder = OpenOptions::new()
            .write(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(link)
            .unwrap();
        let mut returned = 0_u32;
        // SAFETY: `folder` is a live handle to an empty folder opened for
        // writing; the input buffer is a complete mount point reparse buffer
        // of the length passed; no output buffer; `returned` is a valid out
        // pointer; the call is synchronous.
        let ok = unsafe {
            DeviceIoControl(
                folder.as_raw_handle(),
                FSCTL_SET_REPARSE_POINT,
                buffer.as_ptr().cast(),
                u32::try_from(buffer.len()).unwrap(),
                ptr::null_mut(),
                0,
                &raw mut returned,
                ptr::null_mut(),
            )
        };
        assert_ne!(ok, 0, "FSCTL_SET_REPARSE_POINT failed: {}", last_error());
    }

    /// The final path of an open file is where the file is, not the path it
    /// was opened by: through a junction it is the target's, once the file
    /// has moved it is the new place, and a path past `MAX_PATH` is read
    /// whole.
    #[test]
    fn the_final_path_of_an_open_file_is_where_the_file_is() {
        let scratch = Scratch::new("final-path");
        let real = scratch.0.join("real");
        std::fs::create_dir(&real).unwrap();
        let file = File::create(real.join("file.txt")).unwrap();
        let canonical = std::fs::canonicalize(&real).unwrap();
        assert_eq!(
            final_path_of_file(&file).unwrap(),
            canonical.join("file.txt")
        );

        let link = scratch.0.join("link");
        std::fs::create_dir(&link).unwrap();
        make_junction(&link, &real);
        let through = File::open(link.join("file.txt")).unwrap();
        assert_eq!(
            final_path_of_file(&through).unwrap(),
            canonical.join("file.txt")
        );
        drop(through);
        std::fs::remove_dir(&link).unwrap();

        // `File::create` shares delete access, so the open file can be moved
        // to another folder (its folder cannot, while it is open) and another
        // file put at the path that opened it: the handle still names the
        // file it opened, the path the other one.
        let elsewhere = scratch.0.join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::rename(real.join("file.txt"), elsewhere.join("file.txt")).unwrap();
        std::fs::write(real.join("file.txt"), b"another").unwrap();
        assert_eq!(
            final_path_of_file(&file).unwrap(),
            std::fs::canonicalize(&elsewhere).unwrap().join("file.txt")
        );
        assert_eq!(
            std::fs::canonicalize(real.join("file.txt")).unwrap(),
            canonical.join("file.txt")
        );
        drop(file);

        let mut deep = scratch.0.clone();
        for n in 0..6 {
            deep.push(format!("{n}-{}", "d".repeat(80)));
        }
        std::fs::create_dir_all(&deep).unwrap();
        let file = File::create(deep.join("file.txt")).unwrap();
        let found = final_path_of_file(&file).unwrap();
        assert!(found.as_os_str().len() > 500, "{}", found.display());
        assert_eq!(
            found,
            std::fs::canonicalize(&deep).unwrap().join("file.txt")
        );
    }

    #[test]
    fn junctions_are_opened_themselves_and_seen_as_reparse_points() {
        let scratch = Scratch::new("junction");
        let target = scratch.0.join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("file.txt"), b"x").unwrap();
        let link = scratch.0.join("link");
        std::fs::create_dir(&link).unwrap();
        make_junction(&link, &target);
        // Followed, the junction leads to the target's file...
        assert!(link.join("file.txt").is_file());
        // ...but opened without following, it is the junction itself.
        let opened = open_folder_no_follow(&link).unwrap();
        assert!(is_reparse_point(&opened).unwrap());
        assert!(!is_reparse_point(&open_folder_no_follow(&target).unwrap()).unwrap());
        drop(opened);
        // Removes the junction, not the target.
        std::fs::remove_dir(&link).unwrap();
        assert!(target.join("file.txt").is_file());
    }

    /// A pipe this (non-SYSTEM) account serves under a test name, as a
    /// squatter would serve the watchdog's. Its descriptor names this
    /// account as owner, so the owner is known whether or not the test runs
    /// elevated.
    #[test]
    fn a_pipe_not_owned_by_system_is_refused_before_anything_is_written() {
        let me = current_identity().unwrap();
        let sddl = format!("O:{0}D:P(A;;GA;;;{0})", me.sid);
        let name = format!(r"\\.\pipe\wgal-test-sys-owner-{}", std::process::id());
        let mut listener = PipeListener::bind(&name, &sddl, 2).unwrap();
        let (accepted, read_back) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut connection = listener.accept().unwrap().unwrap();
            let mut buf = [0_u8; 16];
            let read = connection.read(&mut buf, Instant::now() + Duration::from_secs(5));
            accepted.send(read.map_err(|e| e.kind())).unwrap();
        });
        // Lets the server's connect be pending first: a client that opens
        // and closes before that is skipped by `accept`, by design.
        std::thread::sleep(Duration::from_millis(300));

        let refused = open_pipe_owned_by(&name, LOCAL_SYSTEM_SID).unwrap_err();
        assert!(is_not_the_watchdog(&refused));
        assert_eq!(refused.kind(), io::ErrorKind::ConnectionRefused);
        assert_eq!(refused.raw_os_error(), None);
        assert_eq!(
            refused.to_string(),
            "the pipe is not served by the WGAL service"
        );
        // The refused client closed its end without writing: the server
        // read the end of the stream, not a byte.
        let read = read_back.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(read, Ok(0));
        server.join().unwrap();

        // The same open on the same kind of pipe, expecting the owner it
        // really has, connects: the owner is the only thing that refused.
        let name = format!(r"\\.\pipe\wgal-test-sys-owner-ok-{}", std::process::id());
        let listener = PipeListener::bind(&name, &sddl, 2).unwrap();
        let pipe = open_pipe_owned_by(&name, &me.sid).unwrap();
        assert_eq!(object_security(&pipe).unwrap().owner, me.sid);
        drop((pipe, listener));
        assert!(!is_not_the_watchdog(&io::Error::other("anything else")));
    }

    /// What the GUI reads as "not running": with nothing serving the name,
    /// the open fails with the OS's own `ERROR_FILE_NOT_FOUND` (2), not a
    /// refusal of the owner.
    #[test]
    fn a_pipe_nobody_serves_is_the_os_not_found_error() {
        let name = format!(r"\\.\pipe\wgal-test-sys-missing-{}", std::process::id());
        let missing = open_pipe_owned_by(&name, LOCAL_SYSTEM_SID).unwrap_err();
        assert_eq!(missing.raw_os_error(), Some(2));
        assert_eq!(missing.kind(), io::ErrorKind::NotFound);
        assert!(!is_not_the_watchdog(&missing));
    }

    #[test]
    fn appending_never_follows_or_reuses_a_link() {
        use std::io::Write;
        let scratch = Scratch::new("append");
        let path = scratch.0.join("service.log");
        open_append_no_follow(&path)
            .unwrap()
            .write_all(b"one\n")
            .unwrap();
        open_append_no_follow(&path)
            .unwrap()
            .write_all(b"two\n")
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"one\ntwo\n");
        // A folder in its place is refused.
        let folder = scratch.0.join("folder.log");
        std::fs::create_dir(&folder).unwrap();
        assert!(open_append_no_follow(&folder).is_err());
        // So is a junction (any account can make one in its own folder).
        let target = scratch.0.join("target");
        std::fs::create_dir(&target).unwrap();
        let link = scratch.0.join("link.log");
        std::fs::create_dir(&link).unwrap();
        make_junction(&link, &target);
        assert!(open_append_no_follow(&link).is_err());
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0);
    }

    #[test]
    fn pipes_refuse_other_names_and_limits() {
        for bad in [
            r"\\.\pipe\",
            r"\\.\pipe\a\b",
            r"\\.\pipe\has space",
            r"\\.\pipe\..\x",
            r"\\server\pipe\wgal",
            r"\\.\PIPE\wgal",
            "wgal",
        ] {
            assert_eq!(
                PipeListener::bind(bad, "D:P(A;;GA;;;SY)", 1).map(|_| ()),
                Err(PipeError::InvalidName),
                "{bad:?}"
            );
        }
        let name = format!(r"\\.\pipe\wgal-test-sys-{}", std::process::id());
        for limit in [0, MAX_PIPE_INSTANCES + 1] {
            assert_eq!(
                PipeListener::bind(&name, "D:P(A;;GA;;;SY)", limit).map(|_| ()),
                Err(PipeError::InvalidLimit)
            );
        }
        assert!(matches!(
            PipeListener::bind(&name, "not sddl", 1),
            Err(PipeError::InvalidSecurity(_))
        ));
    }

    #[test]
    fn echoes_loopback_and_checks_the_payload() {
        let reply = echo(LOOPBACK, LOOPBACK, Duration::from_secs(2))
            .map(|r| (r.status, r.from, r.payload_matches));
        assert_eq!(reply, Ok((status::SUCCESS, LOOPBACK, true)));
    }

    fn loopback_luid() -> u64 {
        adapters()
            .unwrap()
            .into_iter()
            .find(|a| a.if_type == 24 && a.ipv4.contains(&LOOPBACK))
            .unwrap()
            .luid
    }

    /// Reads the routing table; checks the shape only and prints nothing,
    /// since the routes are this machine's.
    #[test]
    fn reads_the_default_routes() {
        let routes = default_routes().map_err(|e| e.code);
        assert!(
            routes.as_ref().is_ok_and(|r| r.len() <= MAX_DEFAULT_ROUTES),
            "reading the routing table failed"
        );
    }

    /// Reads a neighbor entry that cannot exist (a documentation address on
    /// the loopback interface): nothing is sent, nothing changes.
    #[test]
    fn a_missing_neighbor_entry_is_not_static() {
        let entry = neighbor_is_static(loopback_luid(), Ipv4Addr::new(192, 0, 2, 123).into());
        assert!(
            matches!(
                entry,
                Ok(false)
                    | Err(NetError {
                        code: ERROR_INVALID_PARAMETER
                    })
            ),
            "{entry:?}"
        );
    }

    /// The same for an IPv6 address (documentation range) on loopback.
    #[test]
    fn a_missing_ipv6_neighbor_entry_is_not_static() {
        let address = Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 0x123);
        let entry = neighbor_is_static(loopback_luid(), address.into());
        assert!(
            matches!(
                entry,
                Ok(false)
                    | Err(NetError {
                        code: ERROR_INVALID_PARAMETER
                    })
            ),
            "{entry:?}"
        );
    }

    /// The loopback interface has no link layer, so 127.0.0.1 never answers
    /// as a neighbor; nothing leaves the host. The documentation says such a
    /// request is refused as invalid; Windows 11 10.0.26200 reports it
    /// unanswered instead. Either is no answer.
    #[test]
    fn loopback_never_answers_as_a_neighbor() {
        let answer = resolve_neighbor(loopback_luid(), LOOPBACK.into());
        assert!(
            matches!(
                answer,
                Ok(false)
                    | Err(NetError {
                        code: ERROR_INVALID_PARAMETER
                    })
            ),
            "{answer:?}"
        );
    }

    /// The same for ::1: no neighbor discovery on loopback, nothing leaves
    /// the host.
    #[test]
    fn loopback_never_answers_as_an_ipv6_neighbor() {
        let answer = resolve_neighbor(loopback_luid(), Ipv6Addr::LOCALHOST.into());
        assert!(
            matches!(
                answer,
                Ok(false)
                    | Err(NetError {
                        code: ERROR_INVALID_PARAMETER
                    })
            ),
            "{answer:?}"
        );
    }

    /// Registers for interface notices and cancels; no network change is
    /// caused. Ignored only because it registers with the real IP Helper:
    /// `cargo test -p wgal-platform --locked
    /// windows::sys::tests::registers_and_cancels_interface_notices --
    /// --ignored`.
    #[test]
    #[ignore = "registers a real IP Helper notification; run on its own"]
    fn registers_and_cancels_interface_notices() {
        let (watch, notices) = InterfaceWatch::start().unwrap();
        drop(watch);
        // Notices of a change that happened meanwhile may still be queued;
        // then the channel ends, because the registration was cancelled.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match notices.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(_) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => panic!("the channel stayed open"),
            }
        }
    }
}
