//! Win32 helpers shared by the non-Logitech backends (Bluetooth, game
//! controllers, other brands): device node properties through the
//! configuration manager, HID enumeration and short-lived HID handles with
//! timed reads. Nothing here keeps a thread or polls on its own.

use crate::win::wide;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use windows_sys::core::GUID;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_DevNode_PropertyW, CM_Get_Device_ID_ListW, CM_Get_Device_ID_List_SizeW, CM_Get_Device_IDW,
    CM_Get_Device_Interface_ListW, CM_Get_Device_Interface_List_SizeW, CM_Get_Device_Interface_PropertyW, CM_Get_Parent,
    CM_Locate_DevNodeW, CM_GETIDLIST_FILTER_PRESENT, CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_LOCATE_DEVNODE_NORMAL,
    CR_SUCCESS,
};
use windows_sys::Win32::Devices::HumanInterfaceDevice::{
    HidD_FreePreparsedData, HidD_GetAttributes, HidD_GetFeature, HidD_GetHidGuid, HidD_GetPreparsedData,
    HidD_GetProductString, HidD_GetSerialNumberString, HidD_SetFeature, HidP_GetCaps, HIDD_ATTRIBUTES, HIDP_CAPS,
    HIDP_STATUS_SUCCESS,
};
use windows_sys::Win32::Devices::Properties::{
    DEVPKEY_Device_InstanceId, DEVPROPTYPE, DEVPROP_TYPE_BYTE, DEVPROP_TYPE_GUID, DEVPROP_TYPE_STRING,
    DEVPROP_TYPE_UINT16, DEVPROP_TYPE_UINT32,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, DEVPROPKEY, ERROR_IO_PENDING, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
    WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Threading::WaitForSingleObject;
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

pub fn from_wide(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// Splits a NUL-separated, double-NUL-terminated UTF-16 list.
fn multi_sz(buf: &[u16]) -> Vec<String> {
    buf.split(|&c| c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect()
}

// ------------------------------------------------------ device nodes

/// Paths of every present interface of a device interface class.
pub fn interface_paths(class: &GUID) -> Vec<String> {
    for _ in 0..3 {
        let mut len = 0u32;
        // SAFETY: valid out pointer and class GUID.
        let rc = unsafe { CM_Get_Device_Interface_List_SizeW(&mut len, class, null(), CM_GET_DEVICE_INTERFACE_LIST_PRESENT) };
        if rc != CR_SUCCESS || len <= 1 {
            return Vec::new();
        }
        let mut buf = vec![0u16; len as usize];
        // SAFETY: `buf` holds `len` UTF-16 units.
        let rc = unsafe {
            CM_Get_Device_Interface_ListW(class, null(), buf.as_mut_ptr(), len, CM_GET_DEVICE_INTERFACE_LIST_PRESENT)
        };
        if rc == CR_SUCCESS {
            return multi_sz(&buf);
        }
        // The list grew between the two calls: retry.
    }
    Vec::new()
}

/// Instance ids of every present device node.
pub fn present_device_ids() -> Vec<String> {
    for _ in 0..3 {
        let mut len = 0u32;
        // SAFETY: valid out pointer, no filter string.
        if unsafe { CM_Get_Device_ID_List_SizeW(&mut len, null(), CM_GETIDLIST_FILTER_PRESENT) } != CR_SUCCESS || len <= 1 {
            return Vec::new();
        }
        let mut buf = vec![0u16; len as usize];
        // SAFETY: `buf` holds `len` UTF-16 units.
        if unsafe { CM_Get_Device_ID_ListW(null(), buf.as_mut_ptr(), len, CM_GETIDLIST_FILTER_PRESENT) } == CR_SUCCESS {
            return multi_sz(&buf);
        }
    }
    Vec::new()
}

/// Instance id of the device node that exposes an interface.
pub fn interface_instance_id(path: &str) -> Option<String> {
    let wpath = wide(path);
    let mut buf = [0u16; 512];
    let mut size = (buf.len() * 2) as u32;
    let mut prop_type = 0u32;
    // SAFETY: buffers are valid for the sizes given.
    let rc = unsafe {
        CM_Get_Device_Interface_PropertyW(
            wpath.as_ptr(),
            &DEVPKEY_Device_InstanceId,
            &mut prop_type,
            buf.as_mut_ptr().cast(),
            &mut size,
            0,
        )
    };
    (rc == CR_SUCCESS).then(|| from_wide(&buf))
}

pub fn locate(instance_id: &str) -> Option<u32> {
    let wid = wide(instance_id);
    let mut node = 0u32;
    // SAFETY: valid NUL-terminated id and out pointer.
    (unsafe { CM_Locate_DevNodeW(&mut node, wid.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) } == CR_SUCCESS).then_some(node)
}

pub fn parent(node: u32) -> Option<u32> {
    let mut parent = 0u32;
    // SAFETY: valid out pointer.
    (unsafe { CM_Get_Parent(&mut parent, node, 0) } == CR_SUCCESS).then_some(parent)
}

pub fn node_id(node: u32) -> Option<String> {
    let mut id = [0u16; 512];
    // SAFETY: the buffer length is passed.
    (unsafe { CM_Get_Device_IDW(node, id.as_mut_ptr(), id.len() as u32, 0) } == CR_SUCCESS).then(|| from_wide(&id))
}

/// Instance id of the parent device node of an interface (both HID
/// collections of one physical device share it).
pub fn parent_instance_id(path: &str) -> Option<String> {
    node_id(parent(locate(&interface_instance_id(path)?)?)?)
}

/// Raw device node property of the expected type.
fn prop(node: u32, key: &DEVPROPKEY, expected: DEVPROPTYPE) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; 512];
    let mut size = buf.len() as u32;
    let mut prop_type: DEVPROPTYPE = 0;
    // SAFETY: the buffer is valid for `size` bytes.
    let rc = unsafe { CM_Get_DevNode_PropertyW(node, key, &mut prop_type, buf.as_mut_ptr(), &mut size, 0) };
    if rc != CR_SUCCESS || prop_type != expected {
        return None;
    }
    buf.truncate(size as usize);
    Some(buf)
}

pub fn prop_u8(node: u32, key: &DEVPROPKEY) -> Option<u8> {
    prop(node, key, DEVPROP_TYPE_BYTE)?.first().copied()
}

pub fn prop_u16(node: u32, key: &DEVPROPKEY) -> Option<u16> {
    let b = prop(node, key, DEVPROP_TYPE_UINT16)?;
    Some(u16::from_le_bytes([*b.first()?, *b.get(1)?]))
}

pub fn prop_u32(node: u32, key: &DEVPROPKEY) -> Option<u32> {
    let b = prop(node, key, DEVPROP_TYPE_UINT32)?;
    Some(u32::from_le_bytes(b.get(..4)?.try_into().ok()?))
}

pub fn prop_string(node: u32, key: &DEVPROPKEY) -> Option<String> {
    let b = prop(node, key, DEVPROP_TYPE_STRING)?;
    let units: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    Some(from_wide(&units).trim().to_string()).filter(|s| !s.is_empty())
}

/// GUID property, in its in-memory (mixed endian) layout.
pub fn prop_guid(node: u32, key: &DEVPROPKEY) -> Option<[u8; 16]> {
    prop(node, key, DEVPROP_TYPE_GUID)?.get(..16)?.try_into().ok()
}

// -------------------------------------------------------------- HID

pub struct OwnedHandle(pub HANDLE);

// SAFETY: kernel handles may be used from any thread.
unsafe impl Send for OwnedHandle {}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: we own the handle.
        unsafe { CloseHandle(self.0) };
    }
}

pub fn open(path: &str, access: u32, flags: u32) -> Option<OwnedHandle> {
    // SAFETY: valid NUL-terminated path.
    let h = unsafe {
        CreateFileW(wide(path).as_ptr(), access, FILE_SHARE_READ | FILE_SHARE_WRITE, null(), OPEN_EXISTING, flags, null_mut())
    };
    (h != INVALID_HANDLE_VALUE && !h.is_null()).then(|| OwnedHandle(h))
}

/// One HID top-level collection.
#[derive(Clone, Debug)]
pub struct HidInfo {
    pub path: String,
    pub vid: u16,
    pub pid: u16,
    pub usage_page: u16,
    pub usage: u16,
    pub input_len: usize,
    pub output_len: usize,
    pub feature_len: usize,
    pub product: Option<String>,
    pub serial: Option<String>,
}

fn hid_string(handle: HANDLE, f: unsafe extern "system" fn(HANDLE, *mut core::ffi::c_void, u32) -> bool) -> Option<String> {
    let mut buf = [0u16; 128];
    // SAFETY: the buffer is valid for the byte length given.
    let ok = unsafe { f(handle, buf.as_mut_ptr().cast(), (buf.len() * 2) as u32) };
    if !ok {
        return None;
    }
    Some(from_wide(&buf).trim().to_string()).filter(|s| !s.is_empty())
}

/// Every present HID collection whose vendor id passes `want` (checked on
/// the path first, so other devices are not even opened).
pub fn hid_devices(want: impl Fn(u16) -> bool) -> Vec<HidInfo> {
    let mut guid: GUID = GUID { data1: 0, data2: 0, data3: 0, data4: [0; 8] };
    // SAFETY: `guid` is a valid out pointer.
    unsafe { HidD_GetHidGuid(&mut guid) };
    let mut out = Vec::new();
    for path in interface_paths(&guid) {
        if crate::vendors::path_vid(&path).is_some_and(|vid| !want(vid)) {
            continue;
        }
        // Access 0 is enough to query the descriptor and never fails with
        // "access denied" on collections owned by the system.
        let Some(handle) = open(&path, 0, 0) else { continue };
        // SAFETY: the handle is valid; all structures are plain data.
        unsafe {
            let mut attrs: HIDD_ATTRIBUTES = zeroed();
            attrs.Size = size_of::<HIDD_ATTRIBUTES>() as u32;
            if !HidD_GetAttributes(handle.0, &mut attrs) || !want(attrs.VendorID) {
                continue;
            }
            let mut preparsed = 0;
            if !HidD_GetPreparsedData(handle.0, &mut preparsed) {
                continue;
            }
            let mut caps: HIDP_CAPS = zeroed();
            let status = HidP_GetCaps(preparsed, &mut caps);
            HidD_FreePreparsedData(preparsed);
            if status != HIDP_STATUS_SUCCESS {
                continue;
            }
            out.push(HidInfo {
                vid: attrs.VendorID,
                pid: attrs.ProductID,
                usage_page: caps.UsagePage,
                usage: caps.Usage,
                input_len: caps.InputReportByteLength as usize,
                output_len: caps.OutputReportByteLength as usize,
                feature_len: caps.FeatureReportByteLength as usize,
                product: hid_string(handle.0, HidD_GetProductString),
                serial: hid_string(handle.0, HidD_GetSerialNumberString),
                path,
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// A HID collection opened for overlapped reads / writes. Every operation
/// is bounded by a timeout; the handle is closed on drop.
pub struct Hid {
    handle: OwnedHandle,
    event: crate::win::Event,
    input_len: usize,
    output_len: usize,
}

impl Hid {
    pub fn open(info: &HidInfo, write: bool) -> Option<Self> {
        let access = if write { GENERIC_READ | GENERIC_WRITE } else { GENERIC_READ };
        let handle = open(&info.path, access, FILE_FLAG_OVERLAPPED)?;
        Some(Self { handle, event: crate::win::Event::new(true).ok()?, input_len: info.input_len, output_len: info.output_len })
    }

    /// Opens a collection for feature reports only. Keyboards and mice are
    /// owned by the system and cannot be opened for reading or writing, but
    /// a handle without access rights still exchanges feature reports.
    pub fn open_features(info: &HidInfo) -> Option<Self> {
        let handle = open(&info.path, GENERIC_READ | GENERIC_WRITE, 0).or_else(|| open(&info.path, 0, 0))?;
        Some(Self { handle, event: crate::win::Event::new(true).ok()?, input_len: info.input_len, output_len: info.output_len })
    }

    /// Waits for an overlapped operation; cancels it after `ms`.
    /// SAFETY: `overlapped` (and the buffer it refers to) must stay alive
    /// until this returns, which it does only once the I/O is finished.
    unsafe fn complete(&self, ok: i32, overlapped: &OVERLAPPED, ms: u32) -> Option<usize> {
        let mut n = 0u32;
        // SAFETY: see the function contract.
        unsafe {
            if ok == 0 {
                if GetLastError() != ERROR_IO_PENDING {
                    return None;
                }
                if WaitForSingleObject(self.event.0, ms) != WAIT_OBJECT_0 {
                    CancelIoEx(self.handle.0, overlapped);
                }
            }
            (GetOverlappedResult(self.handle.0, overlapped, &mut n, 1) != 0).then_some(n as usize)
        }
    }

    /// Next input report (first byte = report id), or `None` on timeout.
    pub fn read(&self, ms: u32) -> Option<Vec<u8>> {
        let mut buf = vec![0u8; self.input_len.max(1)];
        // SAFETY: OVERLAPPED is plain data; `buf` and `overlapped` outlive
        // the operation (`complete` waits for it or cancels it).
        unsafe {
            let mut overlapped: OVERLAPPED = zeroed();
            overlapped.hEvent = self.event.0;
            let ok = ReadFile(self.handle.0, buf.as_mut_ptr(), buf.len() as u32, null_mut(), &mut overlapped);
            let n = self.complete(ok, &overlapped, ms)?;
            buf.truncate(n);
        }
        Some(buf)
    }

    /// Sends an output report (padded to the collection's report length).
    pub fn write(&self, report: &[u8]) -> bool {
        let mut buf = vec![0u8; self.output_len.max(report.len())];
        buf[..report.len()].copy_from_slice(report);
        // SAFETY: as in `read`.
        unsafe {
            let mut overlapped: OVERLAPPED = zeroed();
            overlapped.hEvent = self.event.0;
            let ok = WriteFile(self.handle.0, buf.as_ptr(), buf.len() as u32, null_mut(), &mut overlapped);
            self.complete(ok, &overlapped, 1000).is_some()
        }
    }

    /// Sends a feature report (first byte = report id).
    pub fn set_feature(&self, report: &[u8]) -> bool {
        // SAFETY: the buffer is valid for its length.
        unsafe { HidD_SetFeature(self.handle.0, report.as_ptr().cast(), report.len() as u32) }
    }

    /// Reads a feature report into `buf` (`buf[0]` = report id on input).
    pub fn get_feature(&self, buf: &mut [u8]) -> bool {
        // SAFETY: the buffer is valid for its length.
        unsafe { HidD_GetFeature(self.handle.0, buf.as_mut_ptr().cast(), buf.len() as u32) }
    }
}
