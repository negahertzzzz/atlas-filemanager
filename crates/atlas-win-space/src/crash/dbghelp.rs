//! DbgHelp is loaded from System32 at startup and retained for process lifetime.
//! The fault path uses fixed stack buffers and no Rust locks or allocation.
use std::{
    ffi::c_void,
    fmt::Write as _,
    io, mem,
    ptr::{null, null_mut},
    sync::OnceLock,
};
use windows_sys::Win32::{
    Foundation::{BOOL, HANDLE},
    Storage::FileSystem::WriteFile,
    System::{
        Diagnostics::Debug::*,
        LibraryLoader::{
            GetModuleFileNameW, GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
        },
        Threading::{GetCurrentProcess, GetCurrentThread},
    },
};

type Dump = unsafe extern "system" fn(
    HANDLE,
    u32,
    HANDLE,
    MINIDUMP_TYPE,
    *const MINIDUMP_EXCEPTION_INFORMATION,
    *const MINIDUMP_USER_STREAM_INFORMATION,
    *const MINIDUMP_CALLBACK_INFORMATION,
) -> BOOL;
type Walk = unsafe extern "system" fn(
    u32,
    HANDLE,
    HANDLE,
    *mut STACKFRAME64,
    *mut c_void,
    PREAD_PROCESS_MEMORY_ROUTINE64,
    PFUNCTION_TABLE_ACCESS_ROUTINE64,
    PGET_MODULE_BASE_ROUTINE64,
    PTRANSLATE_ADDRESS_ROUTINE64,
) -> BOOL;
type Table = unsafe extern "system" fn(HANDLE, u64) -> *mut c_void;
type Base = unsafe extern "system" fn(HANDLE, u64) -> u64;
type Symbol = unsafe extern "system" fn(HANDLE, u64, *mut u64, *mut SYMBOL_INFOW) -> BOOL;
type SourceLine = unsafe extern "system" fn(HANDLE, u64, *mut u32, *mut IMAGEHLP_LINEW64) -> BOOL;
struct Functions {
    dump: Dump,
    walk: Walk,
    table: Table,
    base: Base,
    symbol: Symbol,
    source_line: SourceLine,
}
static API: OnceLock<Functions> = OnceLock::new();

pub(super) fn load() -> io::Result<()> {
    if API.get().is_some() {
        return Ok(());
    }
    unsafe {
        let module = LoadLibraryExW(
            super::super::wide(std::ffi::OsStr::new("dbghelp.dll")).as_ptr(),
            0,
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        );
        if module == 0 {
            return Err(io::Error::last_os_error());
        }
        // Never unload: callbacks and the exception filter retain these pointers.
        macro_rules! resolve {
            ($name:literal, $ty:ty) => {
                mem::transmute::<unsafe extern "system" fn() -> isize, $ty>(
                    GetProcAddress(module, concat!($name, "\0").as_ptr())
                        .ok_or_else(io::Error::last_os_error)?,
                )
            };
        }
        let functions = Functions {
            dump: resolve!("MiniDumpWriteDump", Dump),
            walk: resolve!("StackWalk64", Walk),
            table: resolve!("SymFunctionTableAccess64", Table),
            base: resolve!("SymGetModuleBase64", Base),
            symbol: resolve!("SymFromAddrW", Symbol),
            source_line: resolve!("SymGetLineFromAddrW64", SourceLine),
        };
        let initialize = resolve!(
            "SymInitializeW",
            unsafe extern "system" fn(HANDLE, *const u16, BOOL) -> BOOL
        );
        let options = resolve!("SymSetOptions", unsafe extern "system" fn(u32) -> u32);
        // Use local PDBs only; an exception must never wait for symbol servers
        // or display DbgHelp's error dialogs.
        options(
            SYMOPT_LOAD_LINES | SYMOPT_UNDNAME | SYMOPT_FAIL_CRITICAL_ERRORS | SYMOPT_NO_PROMPTS,
        );
        let search = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_owned()))
            .map(|p| super::super::wide(p.as_os_str()));
        initialize(
            GetCurrentProcess(),
            search.as_ref().map_or(null(), |p| p.as_ptr()),
            1,
        );
        let _ = API.set(functions);
    }
    Ok(())
}

pub(super) unsafe fn dump(file: HANDLE, pid: u32, info: *const MINIDUMP_EXCEPTION_INFORMATION) {
    if let Some(api) = API.get() {
        unsafe {
            (api.dump)(
                GetCurrentProcess(),
                pid,
                file,
                MiniDumpWithThreadInfo | MiniDumpWithUnloadedModules,
                info,
                null(),
                null(),
            );
        }
    }
}

pub(super) unsafe fn stack(file: HANDLE, exception: *const EXCEPTION_POINTERS) {
    let Some(api) = API.get() else {
        return;
    };
    let mut context: CONTEXT = unsafe { mem::zeroed() };
    if exception.is_null() {
        unsafe {
            RtlCaptureContext(&mut context);
        }
    } else {
        let source = unsafe { (*exception).ContextRecord };
        if source.is_null() {
            return;
        }
        context = unsafe { *source };
    }
    let mut frame: STACKFRAME64 = unsafe { mem::zeroed() };
    #[cfg(target_arch = "x86_64")]
    let machine = {
        frame.AddrPC.Offset = context.Rip;
        frame.AddrFrame.Offset = context.Rbp;
        frame.AddrStack.Offset = context.Rsp;
        0x8664
    };
    #[cfg(target_arch = "aarch64")]
    let machine = {
        frame.AddrPC.Offset = context.Pc;
        frame.AddrStack.Offset = context.Sp;
        frame.AddrFrame.Offset = unsafe { context.Anonymous.Anonymous.Fp };
        0xaa64
    };
    #[cfg(target_arch = "x86")]
    let machine = {
        frame.AddrPC.Offset = u64::from(context.Eip);
        frame.AddrFrame.Offset = u64::from(context.Ebp);
        frame.AddrStack.Offset = u64::from(context.Esp);
        0x014c
    };
    frame.AddrPC.Mode = AddrModeFlat;
    frame.AddrFrame.Mode = AddrModeFlat;
    frame.AddrStack.Mode = AddrModeFlat;
    let mut previous = (0, 0);
    let mut repeated = false;
    for _ in 0..128 {
        if frame.AddrPC.Offset == 0 {
            break;
        }
        let current = (frame.AddrPC.Offset, frame.AddrStack.Offset);
        let duplicate = current == previous;
        if duplicate && repeated {
            break;
        }
        repeated = duplicate;
        previous = current;
        // Always retain addresses if a release build has no local PDB.
        if !duplicate {
            let base = unsafe { (api.base)(GetCurrentProcess(), frame.AddrPC.Offset) };
            let mut line = *b"0x0000000000000000 module=0x0000000000000000";
            hex(&mut line[2..18], frame.AddrPC.Offset);
            hex(&mut line[28..44], base);
            let mut written = 0;
            unsafe {
                WriteFile(
                    file,
                    line.as_ptr(),
                    line.len() as u32,
                    &mut written,
                    null_mut(),
                );
                // The module name lets release stacks (PDB not shipped) be
                // symbolized offline by `scripts/symbolize-crash.py`.
                write_module_name(file, base);
                WriteFile(file, b"\r\n".as_ptr(), 2, &mut written, null_mut());
                write_symbol(file, api, frame.AddrPC.Offset);
            }
        }
        if unsafe {
            (api.walk)(
                machine,
                GetCurrentProcess(),
                GetCurrentThread(),
                &mut frame,
                (&mut context as *mut CONTEXT).cast(),
                None,
                Some(api.table),
                Some(api.base),
                None,
            )
        } == 0
        {
            break;
        }
    }
}

struct FixedLine {
    bytes: [u8; 8192],
    len: usize,
}
impl std::fmt::Write for FixedLine {
    fn write_str(&mut self, value: &str) -> std::fmt::Result {
        let available = self.bytes.len() - self.len;
        let mut count = value.len().min(available);
        while !value.is_char_boundary(count) {
            count -= 1;
        }
        self.bytes[self.len..self.len + count].copy_from_slice(&value.as_bytes()[..count]);
        self.len += count;
        Ok(())
    }
}
impl FixedLine {
    fn wide(&mut self, value: &[u16]) {
        for ch in char::decode_utf16(value.iter().copied()) {
            let mut bytes = [0; 4];
            let _ = self.write_str(
                ch.unwrap_or(char::REPLACEMENT_CHARACTER)
                    .encode_utf8(&mut bytes),
            );
        }
    }
}

unsafe fn write_symbol(file: HANDLE, api: &Functions, address: u64) {
    // u64 storage satisfies SYMBOL_INFOW's alignment and leaves room for its
    // flexible UTF-16 name member. All bounds are local constants.
    let mut storage = [0u64; (mem::size_of::<SYMBOL_INFOW>() + 2048).div_ceil(8)];
    let symbol = storage.as_mut_ptr().cast::<SYMBOL_INFOW>();
    let mut displacement = 0u64;
    let mut output = FixedLine {
        bytes: [0; 8192],
        len: 0,
    };
    unsafe {
        (*symbol).SizeOfStruct = mem::size_of::<SYMBOL_INFOW>() as u32;
        (*symbol).MaxNameLen = 1024;
        if (api.symbol)(GetCurrentProcess(), address, &mut displacement, symbol) != 0 {
            let _ = output.write_str("  ");
            output.wide(std::slice::from_raw_parts(
                (*symbol).Name.as_ptr(),
                ((*symbol).NameLen as usize).min(1023),
            ));
            let _ = write!(output, "+0x{displacement:x}");
        }
        let mut source: IMAGEHLP_LINEW64 = mem::zeroed();
        source.SizeOfStruct = mem::size_of::<IMAGEHLP_LINEW64>() as u32;
        let mut line_displacement = 0;
        if (api.source_line)(
            GetCurrentProcess(),
            address,
            &mut line_displacement,
            &mut source,
        ) != 0
            && !source.FileName.is_null()
        {
            let mut length = 0;
            while length < 2048 && *source.FileName.add(length) != 0 {
                length += 1;
            }
            let _ = output.write_str(" ");
            output.wide(std::slice::from_raw_parts(source.FileName, length));
            let _ = write!(output, ":{}", source.LineNumber);
        }
        if output.len != 0 {
            let _ = output.write_str("\r\n");
            let mut written = 0;
            WriteFile(
                file,
                output.bytes.as_ptr(),
                output.len as u32,
                &mut written,
                null_mut(),
            );
        }
    }
}

/// Appends ` <file name>` of the module loaded at `base`. Fixed buffers only.
unsafe fn write_module_name(file: HANDLE, base: u64) {
    let mut wide = [0u16; 260];
    let len =
        unsafe { GetModuleFileNameW(base as isize, wide.as_mut_ptr(), wide.len() as u32) } as usize;
    if base == 0 || len == 0 {
        return;
    }
    let path = &wide[..len.min(wide.len())];
    let name = path
        .rsplit(|&c| c == u16::from(b'\\'))
        .next()
        .unwrap_or(path);
    let mut output = [0u8; 1024];
    output[0] = b' ';
    let mut used = 1;
    for ch in char::decode_utf16(name.iter().copied()) {
        let mut bytes = [0; 4];
        let encoded = ch
            .unwrap_or(char::REPLACEMENT_CHARACTER)
            .encode_utf8(&mut bytes);
        if used + encoded.len() > output.len() {
            break;
        }
        output[used..used + encoded.len()].copy_from_slice(encoded.as_bytes());
        used += encoded.len();
    }
    let mut written = 0;
    unsafe { WriteFile(file, output.as_ptr(), used as u32, &mut written, null_mut()) };
}

fn hex(output: &mut [u8], value: u64) {
    for (i, byte) in output.iter_mut().enumerate() {
        *byte = b"0123456789abcdef"[((value >> ((15 - i) * 4)) & 15) as usize];
    }
}
