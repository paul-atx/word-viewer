//! word-viewer.exe: the Win32 window. All file parsing lives in the safe library;
//! the unsafe code here only talks to Windows and never touches file contents.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
fn main() {
    win::run();
}

/// Non-Windows builds are a command-line converter, used for testing the parsers.
#[cfg(not(windows))]
fn main() {
    let path = std::env::args().nth(1).expect("usage: word-viewer FILE");
    let data = std::fs::read(&path).expect("read failed");
    match word_viewer::to_rtf_named(&data, &path).ok() {
        Some(rtf) => print!("{rtf}"),
        None => std::process::exit(1),
    }
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::fs::File;
    use std::io::Read;
    use std::os::windows::ffi::OsStringExt;
    use std::ptr::null;
    use std::sync::atomic::{AtomicIsize, Ordering};

    type Hwnd = isize;
    type WndProc = unsafe extern "system" fn(Hwnd, u32, usize, isize) -> isize;

    #[repr(C)]
    struct WndClass {
        style: u32,
        proc_: WndProc,
        cls_extra: i32,
        wnd_extra: i32,
        instance: isize,
        icon: isize,
        cursor: isize,
        background: isize,
        menu_name: *const u16,
        class_name: *const u16,
    }

    #[repr(C)]
    struct Msg {
        hwnd: Hwnd,
        message: u32,
        wparam: usize,
        lparam: isize,
        time: u32,
        pt: [i32; 2],
        private: u32,
    }

    #[repr(C)]
    struct Accel {
        virt: u8,
        key: u16,
        cmd: u16,
    }

    #[repr(C)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    #[repr(C)]
    struct SetTextEx {
        flags: u32,
        codepage: u32,
    }

    #[repr(C)]
    struct OpenFileName {
        size: u32,
        owner: Hwnd,
        instance: isize,
        filter: *const u16,
        custom_filter: *mut u16,
        max_cust_filter: u32,
        filter_index: u32,
        file: *mut u16,
        max_file: u32,
        file_title: *mut u16,
        max_file_title: u32,
        initial_dir: *const u16,
        title: *const u16,
        flags: u32,
        file_offset: u16,
        file_extension: u16,
        def_ext: *const u16,
        cust_data: isize,
        hook: *const c_void,
        template_name: *const u16,
        reserved_ptr: *mut c_void,
        reserved: u32,
        flags_ex: u32,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn SetDefaultDllDirectories(flags: u32) -> i32;
        fn LoadLibraryExW(name: *const u16, file: isize, flags: u32) -> isize;
        fn GetModuleHandleW(name: *const u16) -> isize;
        fn GetCommandLineW() -> *const u16;
        fn LocalFree(mem: *mut c_void) -> *mut c_void;
    }

    #[link(name = "user32")]
    extern "system" {
        fn SetProcessDPIAware() -> i32;
        fn RegisterClassW(wc: *const WndClass) -> u16;
        fn CreateWindowExW(ex: u32, class: *const u16, title: *const u16, style: u32, x: i32, y: i32, w: i32, h: i32,
                           parent: Hwnd, menu: isize, inst: isize, param: *const c_void) -> Hwnd;
        fn DefWindowProcW(h: Hwnd, m: u32, w: usize, l: isize) -> isize;
        fn GetMessageW(msg: *mut Msg, h: Hwnd, min: u32, max: u32) -> i32;
        fn TranslateMessage(msg: *const Msg) -> i32;
        fn DispatchMessageW(msg: *const Msg) -> isize;
        fn CreateAcceleratorTableW(acc: *const Accel, n: i32) -> isize;
        fn TranslateAcceleratorW(h: Hwnd, acc: isize, msg: *const Msg) -> i32;
        fn PostQuitMessage(code: i32);
        fn DestroyWindow(h: Hwnd) -> i32;
        fn MoveWindow(h: Hwnd, x: i32, y: i32, w: i32, h_: i32, repaint: i32) -> i32;
        fn SendMessageW(h: Hwnd, m: u32, w: usize, l: isize) -> isize;
        fn SetWindowTextW(h: Hwnd, s: *const u16) -> i32;
        fn ShowWindow(h: Hwnd, cmd: i32) -> i32;
        fn LoadCursorW(inst: isize, name: usize) -> isize;
        fn LoadIconW(inst: isize, name: usize) -> isize;
        fn CreateMenu() -> isize;
        fn CreatePopupMenu() -> isize;
        fn AppendMenuW(menu: isize, flags: u32, id: usize, text: *const u16) -> i32;
    }

    #[link(name = "shell32")]
    extern "system" {
        fn CommandLineToArgvW(cmd: *const u16, argc: *mut i32) -> *mut *mut u16;
        fn DragQueryFileW(drop: isize, i: u32, file: *mut u16, n: u32) -> u32;
        fn DragFinish(drop: isize);
    }

    #[link(name = "comdlg32")]
    extern "system" {
        fn GetOpenFileNameW(ofn: *mut OpenFileName) -> i32;
    }

    const WM_DESTROY: u32 = 0x0002;
    const WM_SIZE: u32 = 0x0005;
    const WM_COMMAND: u32 = 0x0111;
    const WM_DROPFILES: u32 = 0x0233;
    const EM_SETSEL: u32 = 0x00B1;
    const EM_SETRECT: u32 = 0x00B3;
    const EM_SCROLLCARET: u32 = 0x00B7;
    const EM_EXLIMITTEXT: u32 = 0x0435;
    const EM_SETTEXTEX: u32 = 0x0461;
    const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0800;
    const ID_OPEN: usize = 1;
    const ID_EXIT: usize = 2;
    const MAX_PATH: usize = 260;

    static WIN: AtomicIsize = AtomicIsize::new(0);
    static EDIT: AtomicIsize = AtomicIsize::new(0);

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    /// Length of a NUL-terminated wide string, looking at most `max` units.
    unsafe fn wlen(p: *const u16, max: usize) -> usize {
        (0..max).position(|i| *p.add(i) == 0).unwrap_or(max)
    }

    fn show_rtf(rtf: &str) {
        let mut buf = rtf.as_bytes().to_vec(); // RTF output is pure ASCII
        buf.push(0);
        let st = SetTextEx { flags: 0, codepage: 0 };
        let edit = EDIT.load(Ordering::Relaxed);
        unsafe {
            SendMessageW(edit, EM_SETTEXTEX, &st as *const _ as usize, buf.as_ptr() as isize);
            SendMessageW(edit, EM_SETSEL, 0, 0);
            SendMessageW(edit, EM_SCROLLCARET, 0, 0);
        }
    }

    fn read_limited(path: &std::path::Path) -> Option<Vec<u8>> {
        let mut f = File::open(path).ok()?;
        // the library applies each format's own limit; PDFs have the largest
        if f.metadata().ok()?.len() > word_viewer::MAX_PDF as u64 {
            return None;
        }
        let mut data = Vec::new();
        f.by_ref().take(word_viewer::MAX_PDF as u64 + 1).read_to_end(&mut data).ok()?;
        Some(data)
    }

    fn load(path_w: &[u16]) {
        let path = std::path::PathBuf::from(std::ffi::OsString::from_wide(path_w));
        let name: String = path.file_name().map(|n| n.to_string_lossy().chars().take(200).collect()).unwrap_or_default();
        // A parser bug becomes "could not read", never a crash.
        let rtf = read_limited(&path).ok_or(word_viewer::ReadError::Unsupported).and_then(|d| {
            std::panic::catch_unwind(|| word_viewer::to_rtf_named(&d, &name)).unwrap_or(Err(word_viewer::ReadError::Unsupported))
        });
        let title = wide(&format!("{name} - Word Viewer"));
        unsafe { SetWindowTextW(WIN.load(Ordering::Relaxed), title.as_ptr()) };
        match rtf {
            Ok(rtf) => show_rtf(&rtf),
            Err(e) => show_rtf(&format!("{{\\rtf1\\fs22 {}}}", e.message())),
        }
    }

    fn open_dialog() {
        let mut file = [0u16; MAX_PATH];
        let filter: Vec<u16> = "Documents (Word, PDF, RTF, ODT, EPUB, HTML, text)\0*.docx;*.doc;*.pdf;*.rtf;*.odt;*.epub;*.html;*.htm;*.txt;*.md\0All files\0*.*\0\0".encode_utf16().collect();
        let mut ofn: OpenFileName = unsafe { std::mem::zeroed() };
        ofn.size = std::mem::size_of::<OpenFileName>() as u32;
        ofn.owner = WIN.load(Ordering::Relaxed);
        ofn.filter = filter.as_ptr();
        ofn.file = file.as_mut_ptr();
        ofn.max_file = MAX_PATH as u32;
        ofn.flags = 0x1000 | 0x0800; // OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST
        if unsafe { GetOpenFileNameW(&mut ofn) } != 0 {
            let n = file.iter().position(|&c| c == 0).unwrap_or(MAX_PATH);
            load(&file[..n]);
        }
    }

    unsafe extern "system" fn proc_(h: Hwnd, m: u32, w: usize, l: isize) -> isize {
        match m {
            WM_SIZE => {
                let (cw, ch) = ((l & 0xFFFF) as i32, ((l >> 16) & 0xFFFF) as i32);
                let edit = EDIT.load(Ordering::Relaxed);
                MoveWindow(edit, 0, 0, cw, ch, 1);
                let r = Rect { left: 48, top: 32, right: cw - 48, bottom: ch - 16 };
                SendMessageW(edit, EM_SETRECT, 0, &r as *const _ as isize);
                0
            }
            WM_COMMAND => {
                match w & 0xFFFF {
                    ID_OPEN => open_dialog(),
                    ID_EXIT => {
                        DestroyWindow(h);
                    }
                    _ => {}
                }
                0
            }
            WM_DROPFILES => {
                let mut file = [0u16; MAX_PATH];
                let n = DragQueryFileW(w as isize, 0, file.as_mut_ptr(), MAX_PATH as u32) as usize;
                if n > 0 && n < MAX_PATH {
                    load(&file[..n]);
                }
                DragFinish(w as isize);
                0
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(h, m, w, l),
        }
    }

    pub fn run() {
        unsafe {
            // Only load DLLs from System32, never from the exe's folder (e.g. Downloads).
            SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32);
            SetProcessDPIAware();
            if LoadLibraryExW(wide("Msftedit.dll").as_ptr(), 0, LOAD_LIBRARY_SEARCH_SYSTEM32) == 0 {
                return;
            }
            let inst = GetModuleHandleW(null());
            let class = wide("WordViewer");
            let wc = WndClass {
                style: 0,
                proc_,
                cls_extra: 0,
                wnd_extra: 0,
                instance: inst,
                icon: LoadIconW(inst, 1),      // the embedded icon (assets/word-viewer.rc)
                cursor: LoadCursorW(0, 32512), // IDC_ARROW
                background: 0,
                menu_name: null(),
                class_name: class.as_ptr(),
            };
            RegisterClassW(&wc);

            let (file, bar) = (CreatePopupMenu(), CreateMenu());
            AppendMenuW(file, 0, ID_OPEN, wide("&Open...\tCtrl+O").as_ptr());
            AppendMenuW(file, 0x0800, 0, null()); // MF_SEPARATOR
            AppendMenuW(file, 0, ID_EXIT, wide("E&xit").as_ptr());
            AppendMenuW(bar, 0x0010, file as usize, wide("&File").as_ptr()); // MF_POPUP

            let win = CreateWindowExW(
                0x10, // WS_EX_ACCEPTFILES
                class.as_ptr(), wide("Word Viewer").as_ptr(),
                0x00CF_0000, // WS_OVERLAPPEDWINDOW
                i32::MIN, i32::MIN, 1000, 1150, // CW_USEDEFAULT
                0, bar, inst, null());
            WIN.store(win, Ordering::Relaxed);
            let edit = CreateWindowExW(
                0, wide("RICHEDIT50W").as_ptr(), wide("").as_ptr(),
                0x4000_0000 | 0x1000_0000 | 0x0020_0000 | 0x4 | 0x800 | 0x40, // child visible vscroll multiline readonly autovscroll
                0, 0, 0, 0, win, 0, inst, null());
            EDIT.store(edit, Ordering::Relaxed);
            SendMessageW(edit, EM_EXLIMITTEXT, 0, 0x7FFF_FFFF);

            let mut argc = 0;
            let argv = CommandLineToArgvW(GetCommandLineW(), &mut argc);
            if !argv.is_null() && argc > 1 {
                let arg = *argv.add(1);
                load(std::slice::from_raw_parts(arg, wlen(arg, 32768)));
            } else {
                show_rtf("{\\rtf1\\fs22 Use File > Open (Ctrl+O), or drag a document onto this window.}");
            }
            if !argv.is_null() {
                LocalFree(argv as *mut c_void);
            }

            ShowWindow(win, 10); // SW_SHOWDEFAULT
            let acc = Accel { virt: 0x01 | 0x08, key: b'O' as u16, cmd: ID_OPEN as u16 }; // FVIRTKEY | FCONTROL
            let hacc = CreateAcceleratorTableW(&acc, 1);
            let mut msg: Msg = std::mem::zeroed();
            while GetMessageW(&mut msg, 0, 0, 0) > 0 {
                if TranslateAcceleratorW(win, hacc, &msg) == 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
    }
}
