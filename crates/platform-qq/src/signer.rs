// SPDX-License-Identifier: AGPL-3.0-only
//! Direct Rust loader for the pinned research signing module. No Node runtime.
//! The algorithm is still a third-party binary, not a Rust reimplementation.
use crate::Result;
use sha2::{Digest, Sha256};
use std::ffi::{CString, c_char, c_int, c_void};
use std::io::Read;
use std::path::Path;
use std::sync::Mutex;

static NATIVE_LOCK: Mutex<()> = Mutex::new(());
const SHA256: &str = "9a39a5c6de727a3def1c6736f9c3fe8cb9147e5bee22cb77538511a4016b5566";
const OFFSET: usize = 0x5ade220;

#[repr(C)]
struct LinkMap {
    address: usize,
    name: *mut c_char,
    dynamic: *mut c_void,
    next: *mut LinkMap,
    previous: *mut LinkMap,
}

#[link(name = "dl")]
unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlinfo(handle: *mut c_void, request: c_int, info: *mut c_void) -> c_int;
}

#[unsafe(no_mangle)]
pub extern "C" fn qq_magic_napi_register(_: *mut c_void) {}

#[unsafe(export_name = "_ZTVN4vips7VOptionE")]
pub static mut UNUSED_VIPS_VTABLE: [usize; 16] = [0; 16];

type SignFn = unsafe extern "C" fn(*const c_char, *const u8, c_int, c_int, *mut u8);
pub struct Signer {
    call: SignFn,
}
pub struct Signature {
    pub sign: Vec<u8>,
    pub token: Vec<u8>,
    pub extra: Vec<u8>,
}

fn open_library(path: &str, mode: c_int) -> Result<*mut c_void> {
    let path = CString::new(path).map_err(|_| "invalid library path")?;
    let handle = unsafe { dlopen(path.as_ptr(), mode) };
    if handle.is_null() {
        return Err("native signing dependency or exported host shims unavailable".into());
    }
    Ok(handle)
}

pub fn needs_signature(command: &str) -> bool {
    matches!(
        command,
        "OidbSvcTrpcTcp.0x6d9_4"
            | "OidbSvcTrpcTcp.0x88d_0"
            | "MessageSvc.PbSendMsg"
            | "wtlogin.trans_emp"
            | "wtlogin.login"
            | "trpc.login.ecdh.EcdhService.SsoNTLoginEasyLogin"
            | "trpc.login.ecdh.EcdhService.SsoNTLoginPasswordLogin"
            | "trpc.login.ecdh.EcdhService.SsoNTLoginPasswordLoginNewDevice"
    )
}

impl Signer {
    #[cfg(test)]
    pub(crate) fn fixture() -> Self {
        unsafe extern "C" fn synthetic(
            _: *const c_char,
            _: *const u8,
            _: c_int,
            _: c_int,
            output: *mut u8,
        ) {
            // Test-only protocol peer accepts a synthetic signature; no native
            // module is loaded and this path is absent from production builds.
            unsafe {
                *output.add(0x200) = 1;
                *output.add(0x2ff) = 1;
            }
        }
        Self { call: synthetic }
    }
    pub fn load(module: &Path) -> Result<Self> {
        if !cfg!(all(
            target_os = "linux",
            target_arch = "x86_64",
            target_env = "gnu"
        )) {
            return Err("pinned signer requires Linux glibc x86_64".into());
        }
        let _guard = NATIVE_LOCK
            .lock()
            .map_err(|_| "native signer state poisoned")?;
        if !module.is_absolute() {
            return Err("native signer path must be absolute".into());
        }
        let canonical =
            std::fs::canonicalize(module).map_err(|_| "native signer path unavailable")?;
        let module = canonical
            .to_str()
            .ok_or("native signer path must be UTF-8")?;
        let mut file =
            std::fs::File::open(module).map_err(|_| "pinned signing module unavailable")?;
        let mut hash = Sha256::new();
        let mut chunk = [0; 32768];
        loop {
            let n = file
                .read(&mut chunk)
                .map_err(|_| "cannot hash signing module")?;
            if n == 0 {
                break;
            }
            hash.update(&chunk[..n]);
        }
        if format!("{:x}", hash.finalize()) != SHA256 {
            return Err("unverified signing module SHA256".into());
        }
        // Handles deliberately live for this process lifetime. Calling dlclose on
        // an opaque module with static state could invalidate callbacks/state.
        open_library("libstdc++.so.6", 2 | 0x100)?;
        open_library("libgnutls.so.30", 2 | 0x100)?;
        let handle = open_library(module, 1)?;
        let mut map: *mut LinkMap = std::ptr::null_mut();
        if unsafe { dlinfo(handle, 2, (&mut map as *mut *mut LinkMap).cast()) } != 0
            || map.is_null()
        {
            return Err("cannot inspect signing module".into());
        }
        let address = unsafe { (*map).address }
            .checked_add(OFFSET)
            .ok_or("signing offset overflow")?;
        let maps = std::fs::read_to_string("/proc/self/maps")
            .map_err(|_| "cannot check signing mapping")?;
        let executable = maps.lines().any(|line| {
            let mut fields = line.split_whitespace();
            let Some(range) = fields.next() else {
                return false;
            };
            let Some(perms) = fields.next() else {
                return false;
            };
            let Some((lo, hi)) = range.split_once('-') else {
                return false;
            };
            let (Ok(lo), Ok(hi)) = (usize::from_str_radix(lo, 16), usize::from_str_radix(hi, 16))
            else {
                return false;
            };
            perms.contains('x') && line.ends_with(module) && (lo..hi).contains(&address)
        });
        if !executable {
            return Err("signing offset is not executable".into());
        }
        let call = unsafe { std::mem::transmute::<usize, SignFn>(address) };
        Ok(Self { call })
    }
    pub fn sign(&mut self, command: &str, sequence: u32, body: &[u8]) -> Result<Signature> {
        let _guard = NATIVE_LOCK
            .lock()
            .map_err(|_| "native signer state poisoned")?;
        if body.len() > crate::wire::MAX_FRAME {
            return Err("signing input length limit".into());
        }
        if !needs_signature(command) {
            return Err("command outside signing allowlist".into());
        }
        let command = CString::new(command).map_err(|_| "invalid signing command")?;
        let length = c_int::try_from(body.len()).map_err(|_| "signing input length limit")?;
        let sequence = c_int::try_from(sequence).map_err(|_| "signing sequence limit")?;
        let mut output = [0; 0x300];
        unsafe {
            (self.call)(
                command.as_ptr(),
                body.as_ptr(),
                length,
                sequence,
                output.as_mut_ptr(),
            )
        };
        if output[0x2ff] == 0 {
            return Err("empty signature".into());
        }
        Ok(Signature {
            sign: output[0x200..0x200 + usize::from(output[0x2ff])].to_vec(),
            token: output[..usize::from(output[0xff])].to_vec(),
            extra: output[0x100..0x100 + usize::from(output[0x1ff])].to_vec(),
        })
    }
}
