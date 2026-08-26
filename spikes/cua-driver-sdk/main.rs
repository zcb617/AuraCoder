#![cfg_attr(not(windows), allow(dead_code))]

use std::{
    env,
    ffi::{c_char, c_void, OsStr},
    mem,
    os::windows::ffi::OsStrExt,
    ptr,
    slice,
    sync::{Condvar, Mutex},
    time::Duration,
};

#[repr(C)]
#[derive(Clone, Copy)]
struct AbiVersion {
    struct_size: u32,
    major: u16,
    minor: u16,
    patch: u16,
    reserved: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Buffer {
    data: *mut u8,
    len: usize,
    capacity: usize,
}

#[repr(C)]
struct Driver;

#[repr(C)]
struct Operation;

type Completion = unsafe extern "C" fn(*mut c_void, i32, Buffer, Buffer);
type AbiVersionFn = unsafe extern "C" fn(*mut AbiVersion) -> i32;
type AbiCompatibleFn = unsafe extern "C" fn(u16, u16) -> bool;
type BufferFreeFn = unsafe extern "C" fn(*mut Buffer);
type CreateFn = unsafe extern "C" fn(*const u8, usize, *mut *mut Driver, *mut Buffer) -> i32;
type DestroyFn = unsafe extern "C" fn(*mut *mut Driver);
type AvailableFn = unsafe extern "C" fn(*mut Driver, *mut bool, *mut Buffer) -> i32;
type JsonFn = unsafe extern "C" fn(*mut Driver, *mut Buffer, *mut Buffer) -> i32;
type InvokeFn = unsafe extern "C" fn(
    *mut Driver,
    *const u8,
    usize,
    *const u8,
    usize,
    Option<Completion>,
    *mut c_void,
    *mut *mut Operation,
    *mut Buffer,
) -> i32;
type ShutdownFn = unsafe extern "C" fn(
    *mut Driver,
    Option<Completion>,
    *mut c_void,
    *mut *mut Operation,
    *mut Buffer,
) -> i32;
type OperationReleaseFn = unsafe extern "C" fn(*mut *mut Operation);

struct Api {
    module: *mut c_void,
    abi_version: AbiVersionFn,
    abi_compatible: AbiCompatibleFn,
    buffer_free: BufferFreeFn,
    create: CreateFn,
    destroy: DestroyFn,
    available: AvailableFn,
    metadata: JsonFn,
    list_tools: JsonFn,
    invoke: InvokeFn,
    shutdown: ShutdownFn,
    operation_release: OperationReleaseFn,
}

unsafe impl Send for Api {}
unsafe impl Sync for Api {}

struct CompletionState {
    value: Mutex<Option<(i32, String, String)>>,
    wake: Condvar,
}

struct CompletionContext {
    api: *const Api,
    state: *const CompletionState,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    fn FreeLibrary(module: *mut c_void) -> i32;
}

unsafe fn load_symbol<T>(module: *mut c_void, name: &'static [u8]) -> Result<T, String> {
    let address = GetProcAddress(module, name.as_ptr() as *const c_char);
    if address.is_null() {
        return Err(format!("missing symbol {}", String::from_utf8_lossy(name)));
    }
    Ok(mem::transmute_copy(&address))
}

unsafe fn load_api(path: &str) -> Result<Api, String> {
    let wide = OsStr::new(path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let module = LoadLibraryW(wide.as_ptr());
    if module.is_null() {
        return Err(format!("failed to load native library: {path}"));
    }

    let result = (|| {
        Ok(Api {
            module,
            abi_version: load_symbol(module, b"cua_driver_abi_version_v1\0")?,
            abi_compatible: load_symbol(module, b"cua_driver_abi_is_compatible_v1\0")?,
            buffer_free: load_symbol(module, b"cua_driver_buffer_free_v1\0")?,
            create: load_symbol(module, b"cua_driver_create_v1\0")?,
            destroy: load_symbol(module, b"cua_driver_destroy_v1\0")?,
            available: load_symbol(module, b"cua_driver_is_available_v1\0")?,
            metadata: load_symbol(module, b"cua_driver_metadata_json_v1\0")?,
            list_tools: load_symbol(module, b"cua_driver_list_tools_json_v1\0")?,
            invoke: load_symbol(module, b"cua_driver_invoke_v1\0")?,
            shutdown: load_symbol(module, b"cua_driver_shutdown_v1\0")?,
            operation_release: load_symbol(module, b"cua_driver_operation_release_v1\0")?,
        })
    })();

    if result.is_err() {
        let _ = FreeLibrary(module);
    }
    result
}

unsafe fn read_buffer(api: &Api, mut buffer: Buffer) -> String {
    let value = if buffer.data.is_null() || buffer.len == 0 {
        String::new()
    } else {
        String::from_utf8_lossy(slice::from_raw_parts(buffer.data, buffer.len)).into_owned()
    };
    (api.buffer_free)(&mut buffer);
    value
}

unsafe extern "C" fn completion_callback(
    context: *mut c_void,
    status: i32,
    result: Buffer,
    error: Buffer,
) {
    if context.is_null() {
        return;
    }
    let context = &*(context as *const CompletionContext);
    let api = &*context.api;
    let result = read_buffer(api, result);
    let error = read_buffer(api, error);
    let state = &*context.state;
    *state.value.lock().expect("completion state poisoned") = Some((status, result, error));
    state.wake.notify_one();
}

fn wait_for_completion(state: &CompletionState) -> Result<(i32, String, String), String> {
    let guard = state
        .value
        .lock()
        .map_err(|_| "completion state poisoned".to_string())?;
    let (guard, result) = state
        .wake
        .wait_timeout_while(guard, Duration::from_secs(30), |value| value.is_none())
        .map_err(|_| "completion wait failed".to_string())?;
    if result.timed_out() {
        return Err("native operation timed out after 30 seconds".to_string());
    }
    guard
        .clone()
        .ok_or_else(|| "native operation completed without a result".to_string())
}

unsafe fn call_json(api: &Api, driver: *mut Driver, call: JsonFn) -> Result<String, String> {
    let mut output = Buffer {
        data: ptr::null_mut(),
        len: 0,
        capacity: 0,
    };
    let mut error = output;
    let status = call(driver, &mut output, &mut error);
    let output = read_buffer(api, output);
    let error = read_buffer(api, error);
    if status != 0 {
        return Err(format!("native JSON call failed with status {status}: {error}"));
    }
    Ok(output)
}

unsafe fn call_async(
    api: &Api,
    driver: *mut Driver,
    name: Option<&[u8]>,
    arguments: Option<&[u8]>,
    shutdown: bool,
) -> Result<(i32, String, String), String> {
    let state = CompletionState {
        value: Mutex::new(None),
        wake: Condvar::new(),
    };
    let context = CompletionContext {
        api: api as *const Api,
        state: &state as *const CompletionState,
    };
    let mut operation = ptr::null_mut();
    let mut error = Buffer {
        data: ptr::null_mut(),
        len: 0,
        capacity: 0,
    };
    let status = if shutdown {
        (api.shutdown)(
            driver,
            Some(completion_callback),
            &context as *const CompletionContext as *mut c_void,
            &mut operation,
            &mut error,
        )
    } else {
        let name = name.expect("invoke name is required");
        let arguments = arguments.expect("invoke arguments are required");
        (api.invoke)(
            driver,
            name.as_ptr(),
            name.len(),
            arguments.as_ptr(),
            arguments.len(),
            Some(completion_callback),
            &context as *const CompletionContext as *mut c_void,
            &mut operation,
            &mut error,
        )
    };
    let immediate_error = read_buffer(api, error);
    if status != 0 {
        if !operation.is_null() {
            (api.operation_release)(&mut operation);
        }
        return Err(format!("native operation admission failed with status {status}: {immediate_error}"));
    }
    let completed = wait_for_completion(&state);
    if !operation.is_null() {
        (api.operation_release)(&mut operation);
    }
    completed
}

#[cfg(windows)]
fn run(path: &str) -> Result<(), String> {
    unsafe {
        let api = load_api(path)?;
        let mut version = AbiVersion {
            struct_size: mem::size_of::<AbiVersion>() as u32,
            major: 0,
            minor: 0,
            patch: 0,
            reserved: 0,
        };
        let version_status = (api.abi_version)(&mut version);
        let compatible = (api.abi_compatible)(version.major, version.minor);
        let rejects_next_major = !(api.abi_compatible)(version.major.saturating_add(1), 0);
        println!(
            "ABI status={version_status} version={}.{}.{} compatible={compatible} rejects_next_major={rejects_next_major}",
            version.major,
            version.minor,
            version.patch
        );

        let options = b"{}";
        let mut driver = ptr::null_mut();
        let mut create_error = Buffer {
            data: ptr::null_mut(),
            len: 0,
            capacity: 0,
        };
        let create_status = (api.create)(
            options.as_ptr(),
            options.len(),
            &mut driver,
            &mut create_error,
        );
        let create_error = read_buffer(&api, create_error);
        println!("CREATE status={create_status} error={create_error}");
        if create_status != 0 || driver.is_null() {
            let _ = FreeLibrary(api.module);
            return Err("runtime creation failed".to_string());
        }

        let mut available = false;
        let mut available_error = Buffer {
            data: ptr::null_mut(),
            len: 0,
            capacity: 0,
        };
        let available_status = (api.available)(driver, &mut available, &mut available_error);
        let available_error = read_buffer(&api, available_error);
        println!("AVAILABLE status={available_status} value={available} error={available_error}");

        let metadata = call_json(&api, driver, api.metadata)?;
        println!("METADATA {metadata}");
        let tools = call_json(&api, driver, api.list_tools)?;
        println!("TOOLS {tools}");

        let action_result = call_async(
            &api,
            driver,
            Some(b"get_screen_size"),
            Some(b"{}"),
            false,
        );
        println!("GET_SCREEN_SIZE {action_result:?}");

        let shutdown_result = call_async(&api, driver, None, None, true);
        println!("SHUTDOWN {shutdown_result:?}");
        (api.destroy)(&mut driver);
        let _ = FreeLibrary(api.module);
        if shutdown_result.is_err() {
            return Err("runtime shutdown failed".to_string());
        }
        Ok(())
    }
}

#[cfg(not(windows))]
fn run(_path: &str) -> Result<(), String> {
    Err("this spike is Windows-only".to_string())
}

fn main() {
    let path = env::args()
        .nth(1)
        .unwrap_or_else(|| "cua_driver_sdk.dll".to_string());
    if let Err(error) = run(&path) {
        eprintln!("SPIKE_FAILED {error}");
        std::process::exit(1);
    }
    println!("SPIKE_PASSED");
}
