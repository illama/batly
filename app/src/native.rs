//! Native HID access on Windows, without hidapi.dll: device interface
//! enumeration through the configuration manager, overlapped I/O for the
//! reports and one protocol [`Session`](crate::hidpp::Session) thread per
//! physical endpoint. Idle threads sleep in `WaitForMultipleObjects`; there
//! is no periodic wake-up.

use crate::config;
use crate::hidpp::{self, Command, Incoming, Session, Transport, LONG_LEN, REPORT_LONG, REPORT_SHORT, SHORT_LEN};
use crate::log;
use crate::model::Hub;
use crate::win::{wide, Event};
use std::collections::HashMap;
use std::io;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;
use windows_sys::core::GUID;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_Device_IDW, CM_Get_Device_Interface_ListW, CM_Get_Device_Interface_List_SizeW,
    CM_Get_Device_Interface_PropertyW, CM_Get_Parent, CM_Locate_DevNodeW, CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
    CM_LOCATE_DEVNODE_NORMAL, CR_SUCCESS,
};
use windows_sys::Win32::Devices::HumanInterfaceDevice::{
    HidD_FreePreparsedData, HidD_GetAttributes, HidD_GetHidGuid, HidD_GetPreparsedData, HidP_GetCaps, HIDD_ATTRIBUTES,
    HIDP_CAPS, HIDP_STATUS_SUCCESS,
};
use windows_sys::Win32::Devices::Properties::DEVPKEY_Device_InstanceId;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_IO_PENDING, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Threading::{WaitForMultipleObjects, WaitForSingleObject};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

const LOGITECH_VID: u16 = 0x046D;

// ------------------------------------------------------------ enumeration

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Short,
    Long,
}

struct Interface {
    path: String,
    kind: Kind,
    input_len: usize,
    output_len: usize,
    /// Parent device node: both HID++ collections of an endpoint share it.
    group: String,
}

fn from_wide(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// Paths of every present HID device interface.
fn hid_interface_paths() -> Vec<String> {
    let mut guid: GUID = unsafe { zeroed() };
    // SAFETY: `guid` is a valid out pointer.
    unsafe { HidD_GetHidGuid(&mut guid) };
    for _ in 0..3 {
        let mut len = 0u32;
        // SAFETY: valid out pointer.
        let rc = unsafe {
            CM_Get_Device_Interface_List_SizeW(&mut len, &guid, null(), CM_GET_DEVICE_INTERFACE_LIST_PRESENT)
        };
        if rc != CR_SUCCESS || len <= 1 {
            return Vec::new();
        }
        let mut buf = vec![0u16; len as usize];
        // SAFETY: `buf` holds `len` UTF-16 units.
        let rc = unsafe {
            CM_Get_Device_Interface_ListW(&guid, null(), buf.as_mut_ptr(), len, CM_GET_DEVICE_INTERFACE_LIST_PRESENT)
        };
        if rc == CR_SUCCESS {
            return buf.split(|&c| c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect();
        }
        // The list grew between the two calls (CR_BUFFER_SMALL): retry.
    }
    Vec::new()
}

/// Instance id of the parent device node of an interface.
fn parent_instance_id(path: &str) -> Option<String> {
    let wpath = wide(path);
    let mut buf = [0u16; 512];
    let mut size = (buf.len() * 2) as u32;
    let mut prop_type = 0u32;
    // SAFETY: buffers are valid for the sizes given.
    unsafe {
        if CM_Get_Device_Interface_PropertyW(
            wpath.as_ptr(),
            &DEVPKEY_Device_InstanceId,
            &mut prop_type,
            buf.as_mut_ptr().cast(),
            &mut size,
            0,
        ) != CR_SUCCESS
        {
            return None;
        }
        let mut node = 0u32;
        if CM_Locate_DevNodeW(&mut node, buf.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) != CR_SUCCESS {
            return None;
        }
        let mut parent = 0u32;
        if CM_Get_Parent(&mut parent, node, 0) != CR_SUCCESS {
            return None;
        }
        let mut id = [0u16; 512];
        if CM_Get_Device_IDW(parent, id.as_mut_ptr(), id.len() as u32, 0) != CR_SUCCESS {
            return None;
        }
        Some(from_wide(&id).to_ascii_lowercase())
    }
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: we own the handle.
        unsafe { CloseHandle(self.0) };
    }
}

fn open(path: &str, access: u32, flags: u32) -> Option<OwnedHandle> {
    // SAFETY: valid NUL-terminated path.
    let h = unsafe {
        CreateFileW(wide(path).as_ptr(), access, FILE_SHARE_READ | FILE_SHARE_WRITE, null(), OPEN_EXISTING, flags, null_mut())
    };
    (h != INVALID_HANDLE_VALUE && !h.is_null()).then(|| OwnedHandle(h))
}

/// Inspects one HID interface; `None` unless it is a Logitech HID++ one.
fn probe(path: &str) -> Option<Interface> {
    // Access 0 is enough to query the descriptor and never fails with
    // "access denied" on collections owned by the system.
    let handle = open(path, 0, 0)?;
    // SAFETY: the handle is valid; all structures are plain data.
    unsafe {
        let mut attrs: HIDD_ATTRIBUTES = zeroed();
        attrs.Size = size_of::<HIDD_ATTRIBUTES>() as u32;
        if !HidD_GetAttributes(handle.0, &mut attrs) || attrs.VendorID != LOGITECH_VID {
            return None;
        }
        let mut preparsed = 0;
        if !HidD_GetPreparsedData(handle.0, &mut preparsed) {
            return None;
        }
        let mut caps: HIDP_CAPS = zeroed();
        let status = HidP_GetCaps(preparsed, &mut caps);
        HidD_FreePreparsedData(preparsed);
        if status != HIDP_STATUS_SUCCESS {
            return None;
        }
        let kind = classify(caps.UsagePage, caps.Usage, caps.InputReportByteLength as usize)?;
        Some(Interface {
            path: path.to_string(),
            kind,
            input_len: caps.InputReportByteLength as usize,
            output_len: caps.OutputReportByteLength as usize,
            group: parent_instance_id(path).unwrap_or_else(|| path.to_ascii_lowercase()),
        })
    }
}

/// HID++ collections live on a vendor usage page: usage 1 carries the
/// short (7 byte) report, usage 2 the long (20 byte) one. Bluetooth LE
/// devices expose a single long collection (page 0xFF43, usage 0x0202).
fn classify(usage_page: u16, usage: u16, input_len: usize) -> Option<Kind> {
    if usage_page & 0xFF00 != 0xFF00 {
        return None;
    }
    match usage {
        0x0001 if input_len >= SHORT_LEN => Some(Kind::Short),
        0x0002 | 0x0202 if input_len >= LONG_LEN => Some(Kind::Long),
        _ => None,
    }
}

/// HID++ interfaces grouped by physical endpoint.
fn enumerate() -> HashMap<String, Vec<Interface>> {
    let mut groups: HashMap<String, Vec<Interface>> = HashMap::new();
    for path in hid_interface_paths() {
        // Cheap pre-filter on the vendor id embedded in the path
        // (`vid_046d` for USB, `vid&02046d` for Bluetooth).
        if !path.to_ascii_lowercase().contains("046d") {
            continue;
        }
        if let Some(interface) = probe(&path) {
            log!("hid: {:?} {} in={} out={}", interface.kind, path, interface.input_len, interface.output_len);
            groups.entry(interface.group.clone()).or_default().push(interface);
        }
    }
    for list in groups.values_mut() {
        list.sort_by(|a, b| a.path.cmp(&b.path));
    }
    groups
}

// -------------------------------------------------------------- transport

struct Channel {
    handle: OwnedHandle,
    event: Event,
    overlapped: Box<OVERLAPPED>,
    buf: Vec<u8>,
    pending: bool,
    output_len: usize,
}

impl Channel {
    fn open(interface: &Interface) -> io::Result<Self> {
        let handle = open(&interface.path, GENERIC_READ | GENERIC_WRITE, FILE_FLAG_OVERLAPPED)
            .ok_or_else(io::Error::last_os_error)?;
        Ok(Self {
            handle,
            event: Event::new(true)?,
            // SAFETY: OVERLAPPED is plain data; all-zero is its initial state.
            overlapped: Box::new(unsafe { zeroed() }),
            buf: vec![0; interface.input_len.max(LONG_LEN)],
            pending: false,
            output_len: interface.output_len,
        })
    }

    /// Makes sure a read is in flight.
    fn arm(&mut self) -> io::Result<()> {
        if self.pending {
            return Ok(());
        }
        *self.overlapped = unsafe { zeroed() };
        self.overlapped.hEvent = self.event.0;
        // SAFETY: `buf` and `overlapped` are heap allocations that outlive
        // the operation (see `Drop`, which cancels and waits).
        let ok = unsafe {
            ReadFile(self.handle.0, self.buf.as_mut_ptr(), self.buf.len() as u32, null_mut(), &mut *self.overlapped)
        };
        if ok == 0 && unsafe { GetLastError() } != ERROR_IO_PENDING {
            return Err(io::Error::last_os_error());
        }
        self.pending = true;
        Ok(())
    }

    /// Collects the result of the completed read.
    fn finish(&mut self) -> io::Result<Option<Vec<u8>>> {
        let mut n = 0u32;
        self.pending = false;
        // SAFETY: the operation was started on this handle / overlapped.
        if unsafe { GetOverlappedResult(self.handle.0, &*self.overlapped, &mut n, 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let data = &self.buf[..(n as usize).min(self.buf.len())];
        // Normalise to the HID++ report sizes; drop anything else.
        Ok(match data.first() {
            Some(&REPORT_SHORT) if data.len() >= SHORT_LEN => Some(data[..SHORT_LEN].to_vec()),
            Some(&REPORT_LONG) if data.len() >= LONG_LEN => Some(data[..LONG_LEN].to_vec()),
            _ => None,
        })
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        if self.pending {
            let mut n = 0u32;
            // SAFETY: cancel the read and wait for it to finish so the
            // kernel no longer references `buf` / `overlapped`.
            unsafe {
                CancelIoEx(self.handle.0, &*self.overlapped);
                GetOverlappedResult(self.handle.0, &*self.overlapped, &mut n, 1);
            }
        }
    }
}

struct WinTransport {
    short: Option<Channel>,
    long: Option<Channel>,
    wake: Arc<Event>,
    write_event: Event,
}

// SAFETY: the transport is moved to its session thread and only used there;
// the raw handles / OVERLAPPED pointers it owns are not shared.
unsafe impl Send for WinTransport {}

impl Transport for WinTransport {
    fn has_short(&self) -> bool {
        self.short.is_some()
    }

    fn write(&mut self, report: &[u8]) -> io::Result<()> {
        let channel = match (report[0], &self.short, &self.long) {
            (REPORT_SHORT, Some(c), _) => c,
            (REPORT_LONG, _, Some(c)) => c,
            _ => return Err(io::Error::new(io::ErrorKind::Unsupported, "no interface for this report")),
        };
        let mut buf = vec![0u8; channel.output_len.max(report.len())];
        buf[..report.len()].copy_from_slice(report);
        // SAFETY: `buf` and `overlapped` stay alive until the operation has
        // completed or has been cancelled below.
        unsafe {
            let mut overlapped: OVERLAPPED = zeroed();
            overlapped.hEvent = self.write_event.0;
            let mut n = 0u32;
            if WriteFile(channel.handle.0, buf.as_ptr(), buf.len() as u32, null_mut(), &mut overlapped) == 0 {
                if GetLastError() != ERROR_IO_PENDING {
                    return Err(io::Error::last_os_error());
                }
                if WaitForSingleObject(self.write_event.0, 1000) != WAIT_OBJECT_0 {
                    CancelIoEx(channel.handle.0, &overlapped);
                }
                if GetOverlappedResult(channel.handle.0, &overlapped, &mut n, 1) == 0 {
                    return Err(io::Error::last_os_error());
                }
            }
        }
        Ok(())
    }

    fn read(&mut self, timeout: Duration) -> io::Result<Incoming> {
        let mut handles: [HANDLE; 3] = [self.wake.0, null_mut(), null_mut()];
        let mut count = 1;
        for channel in [&mut self.short, &mut self.long].into_iter().flatten() {
            channel.arm()?;
            handles[count] = channel.event.0;
            count += 1;
        }
        // Round up so a sub-millisecond remainder does not busy-loop.
        let ms = timeout.as_millis().saturating_add(1).min(0x7FFF_FFFF) as u32;
        // SAFETY: `handles[..count]` are valid event handles.
        let rc = unsafe { WaitForMultipleObjects(count as u32, handles.as_ptr(), 0, ms) };
        if rc == WAIT_TIMEOUT {
            return Ok(Incoming::Timeout);
        }
        if rc == WAIT_OBJECT_0 {
            return Ok(Incoming::Interrupted);
        }
        let signalled = rc.wrapping_sub(WAIT_OBJECT_0) as usize;
        if signalled >= count {
            return Err(io::Error::last_os_error());
        }
        let channel = [&mut self.short, &mut self.long]
            .into_iter()
            .flatten()
            .find(|c| c.event.0 == handles[signalled])
            .expect("signalled handle belongs to a channel");
        Ok(match channel.finish()? {
            Some(report) => Incoming::Report(report),
            None => Incoming::Timeout,
        })
    }
}

// ---------------------------------------------------------------- manager

struct Running {
    paths: Vec<String>,
    commands: Sender<Command>,
    wake: Arc<Event>,
    thread: JoinHandle<()>,
}

impl Running {
    fn send(&self, command: Command) {
        let _ = self.commands.send(command);
        self.wake.set();
    }
}

enum Request {
    Rescan,
    Refresh,
}

/// Handle to the native device manager thread.
pub struct Native {
    requests: Sender<Request>,
}

impl Native {
    pub fn start(cfg: &config::Native, hub: Arc<Hub>) -> Self {
        let settings = hidpp::Settings {
            retry: Duration::from_secs(cfg.retry_time as u64),
            poll: Duration::from_secs(cfg.poll_period as u64),
            disabled: cfg.disabled_devices.clone(),
            settle: Duration::from_secs(1),
        };
        let (requests, rx) = mpsc::channel();
        let _ = thread::Builder::new().name("hid-manager".into()).stack_size(256 * 1024).spawn(move || {
            let mut running: HashMap<String, Running> = HashMap::new();
            rescan(&mut running, &settings, &hub);
            while let Ok(request) = rx.recv() {
                rescan(&mut running, &settings, &hub);
                if matches!(request, Request::Refresh) {
                    for session in running.values() {
                        session.send(Command::Refresh);
                    }
                }
            }
        });
        Self { requests }
    }

    /// Call after a device arrival / removal.
    pub fn rescan(&self) {
        let _ = self.requests.send(Request::Rescan);
    }

    /// Re-reads every battery now.
    pub fn refresh(&self) {
        let _ = self.requests.send(Request::Refresh);
    }
}

fn rescan(running: &mut HashMap<String, Running>, settings: &hidpp::Settings, hub: &Arc<Hub>) {
    let mut groups = enumerate();

    // Drop sessions that ended (device unplugged) or whose interface set
    // changed (second collection showed up late).
    let stale: Vec<String> = running
        .iter()
        .filter(|(key, r)| {
            r.thread.is_finished()
                || groups.get(*key).is_none_or(|g| g.iter().map(|i| &i.path).ne(r.paths.iter()))
        })
        .map(|(key, _)| key.clone())
        .collect();
    for key in stale {
        if let Some(session) = running.remove(&key) {
            session.send(Command::Stop);
            let _ = session.thread.join();
        }
    }

    for (key, interfaces) in groups.drain() {
        if running.contains_key(&key) {
            continue;
        }
        match spawn_session(&key, &interfaces, settings, hub) {
            Ok(session) => {
                running.insert(key, session);
            }
            Err(e) => log!("hid: cannot open {key}: {e}"),
        }
    }
}

fn spawn_session(
    key: &str,
    interfaces: &[Interface],
    settings: &hidpp::Settings,
    hub: &Arc<Hub>,
) -> io::Result<Running> {
    let open_kind = |kind: Kind| -> io::Result<Option<Channel>> {
        interfaces.iter().find(|i| i.kind == kind).map(Channel::open).transpose()
    };
    let wake = Arc::new(Event::new(false)?);
    let transport =
        WinTransport { short: open_kind(Kind::Short)?, long: open_kind(Kind::Long)?, wake: wake.clone(), write_event: Event::new(true)? };

    let (commands, rx) = mpsc::channel();
    let hub = hub.clone();
    let tag: String = key.rsplit('\\').next().unwrap_or(key).chars().take(24).collect();
    let session = Session::new(transport, settings.clone(), rx, move |event| hub.publish(event), tag);
    let thread = thread::Builder::new().name("hidpp".into()).stack_size(256 * 1024).spawn(move || session.run())?;
    Ok(Running { paths: interfaces.iter().map(|i| i.path.clone()).collect(), commands, wake, thread })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interface_classification() {
        assert_eq!(classify(0xFF00, 1, 7), Some(Kind::Short));
        assert_eq!(classify(0xFF00, 2, 20), Some(Kind::Long));
        assert_eq!(classify(0xFF43, 0x0202, 20), Some(Kind::Long));
        assert_eq!(classify(0xFF00, 4, 64), None);
        assert_eq!(classify(0x0001, 2, 20), None);
        assert_eq!(classify(0xFF00, 2, 8), None);
    }
}
