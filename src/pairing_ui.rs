//! Native invitation and local-access UI. Every HWND is owned by the UI thread.

use anyhow::{bail, Result};
use tokio::sync::mpsc;

pub struct UiConfig {
    pub invitation: String,
    pub expires_unix: u64,
}

pub enum UiEvent {
    Stop,
    SetAccessEnabled(bool),
    Decide { request_id: String, approve: bool },
}

#[derive(Clone)]
pub struct AccessPrompt {
    pub id: String,
    pub peer: String,
    pub label: String,
    pub code: String,
    pub expires_unix: u64,
    pub elevated: bool,
}

#[cfg(not(windows))]
pub struct UiHandle;

#[cfg(not(windows))]
pub fn start(_config: UiConfig) -> Result<(UiHandle, mpsc::UnboundedReceiver<UiEvent>)> {
    bail!("The pairing window requires an interactive Windows desktop")
}

#[cfg(not(windows))]
impl UiHandle {
    pub fn show_requests(&self, _prompts: Vec<AccessPrompt>) {}
    pub fn close(&self) {}
}

#[cfg(windows)]
mod win {
    use super::*;
    use crate::qr::{matrix, render_png, QrMatrix};
    use std::{
        collections::VecDeque,
        ffi::c_void,
        os::windows::ffi::OsStringExt,
        path::PathBuf,
        ptr::{null, null_mut},
        sync::{
            atomic::{AtomicBool, Ordering},
            mpsc as std_mpsc, Arc, Mutex,
        },
        thread::{self, JoinHandle},
        time::{SystemTime, UNIX_EPOCH},
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::IsWindow;
    use windows_sys::Win32::{
        Foundation::{GlobalFree, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::{
            BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, UpdateWindow,
            COLOR_WINDOW, PAINTSTRUCT,
        },
        System::{
            DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
            LibraryLoader::GetModuleHandleW,
            Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE},
        },
        UI::{
            Controls::{
                Dialogs::{
                    CommDlgExtendedError, GetSaveFileNameW, OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT,
                    OFN_PATHMUSTEXIST, OPENFILENAMEW,
                },
                BST_CHECKED, BST_UNCHECKED,
            },
            Input::KeyboardAndMouse::EnableWindow,
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect,
                GetMessageW, GetSystemMetrics, GetWindowLongPtrW, LoadCursorW, MessageBoxW,
                PostMessageW, PostQuitMessage, RegisterClassW, SendMessageW, SetTimer,
                SetWindowLongPtrW, SetWindowTextW, ShowWindow, TranslateMessage, UnregisterClassW,
                BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX, BS_PUSHBUTTON, CREATESTRUCTW,
                ES_AUTOHSCROLL, ES_READONLY, GWLP_USERDATA, IDC_ARROW, MB_ICONERROR, MB_OK, MSG,
                SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SW_SHOW, WM_APP, WM_CLOSE, WM_COMMAND,
                WM_CREATE, WM_DESTROY, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_TIMER, WNDCLASSW,
                WS_CAPTION, WS_CHILD, WS_EX_CLIENTEDGE, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP,
                WS_VISIBLE,
            },
        },
    };

    pub const ID_COPY: usize = 101;
    pub const ID_SAVE: usize = 102;
    pub const ID_STOP: usize = 103;
    pub const ID_RECEIVE: usize = 104;
    pub const ID_CONFIRM: usize = 105;
    pub const ID_APPROVE: usize = 106;
    pub const ID_DENY: usize = 107;
    const WAKE: u32 = WM_APP + 1;
    const TIMER: usize = 1;
    const CF_UNICODETEXT: u32 = 13;

    enum Command {
        Requests(Vec<AccessPrompt>),
        Close,
    }

    struct Shared {
        inner: Mutex<Mailbox>,
        transferred: AtomicBool,
    }

    struct Mailbox {
        hwnd: Option<isize>,
        queue: VecDeque<Command>,
        closing: bool,
    }

    impl Shared {
        fn send(&self, command: Command) {
            let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if guard.closing {
                return;
            }
            match command {
                Command::Requests(prompts) => {
                    guard
                        .queue
                        .retain(|item| !matches!(item, Command::Requests(_)));
                    guard.queue.push_back(Command::Requests(prompts));
                }
                Command::Close => {
                    guard.closing = true;
                    guard.queue.clear();
                    guard.queue.push_back(Command::Close);
                }
            }
            if let Some(hwnd) = guard.hwnd {
                // HWND is cleared by its owner before destruction under this same lock.
                unsafe { PostMessageW(hwnd as HWND, WAKE, 0, 0) };
            }
        }
    }

    pub struct UiHandle {
        shared: Arc<Shared>,
        thread: Option<JoinHandle<()>>,
    }

    impl UiHandle {
        pub fn show_requests(&self, mut prompts: Vec<AccessPrompt>) {
            prompts.truncate(4);
            self.shared.send(Command::Requests(prompts));
        }

        pub fn close(&self) {
            self.shared.send(Command::Close);
        }
    }

    impl Drop for UiHandle {
        fn drop(&mut self) {
            self.close();
            if let Some(handle) = self.thread.take() {
                // Never block a Tokio worker or the host shutdown on GetMessageW.
                let _ = thread::Builder::new()
                    .name("winremote-ui-join".into())
                    .spawn(move || {
                        let _ = handle.join();
                    });
            }
        }
    }

    struct State {
        config: UiConfig,
        qr: QrMatrix,
        events: mpsc::UnboundedSender<UiEvent>,
        shared: Arc<Shared>,
        invitation_edit: HWND,
        countdown: HWND,
        receive: HWND,
        request_text: HWND,
        code_text: HWND,
        privilege_text: HWND,
        confirm: HWND,
        approve: HWND,
        deny: HWND,
        prompts: Vec<AccessPrompt>,
        active_id: Option<String>,
        closing: bool,
    }

    pub fn start(config: UiConfig) -> Result<(UiHandle, mpsc::UnboundedReceiver<UiEvent>)> {
        let qr = matrix(&config.invitation)?;
        if qr.width == 0 || qr.width.checked_mul(qr.width) != Some(qr.modules.len()) {
            bail!("Invalid invitation QR matrix");
        }
        let shared = Arc::new(Shared {
            inner: Mutex::new(Mailbox {
                hwnd: None,
                queue: VecDeque::new(),
                closing: false,
            }),
            transferred: AtomicBool::new(false),
        });
        let (events, receiver) = mpsc::unbounded_channel();
        let (ready_tx, ready_rx) = std_mpsc::sync_channel(1);
        let thread_shared = shared.clone();
        let thread = thread::Builder::new()
            .name("winremote-pairing-ui".into())
            .spawn(move || {
                run(config, qr, events, thread_shared, ready_tx);
            })?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok((
                UiHandle {
                    shared,
                    thread: Some(thread),
                },
                receiver,
            )),
            Ok(Err(error)) => {
                let _ = thread.join();
                bail!("Cannot open pairing window: {error}")
            }
            Err(_) => {
                let _ = thread.join();
                bail!("Pairing window thread exited before startup")
            }
        }
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }

    fn display_text(value: &str, max: usize) -> String {
        value
            .chars()
            .filter(|c| !c.is_control())
            .take(max)
            .collect()
    }

    // Mirrors the Win32 control factory while keeping all layout values at each call site.
    #[allow(clippy::too_many_arguments)]
    unsafe fn control(
        parent: HWND,
        class: &str,
        text: &str,
        style: u32,
        ex: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        id: usize,
    ) -> HWND {
        let class = wide(class);
        let text = wide(text);
        CreateWindowExW(
            ex,
            class.as_ptr(),
            text.as_ptr(),
            style | WS_CHILD | WS_VISIBLE,
            x,
            y,
            w,
            h,
            parent,
            id as *mut c_void,
            null_mut(),
            null(),
        )
    }

    unsafe fn set_text(hwnd: HWND, text: &str) {
        let text = wide(text);
        SetWindowTextW(hwnd, text.as_ptr());
    }

    fn run(
        config: UiConfig,
        qr: QrMatrix,
        events: mpsc::UnboundedSender<UiEvent>,
        shared: Arc<Shared>,
        ready: std_mpsc::SyncSender<std::result::Result<(), String>>,
    ) {
        unsafe {
            let instance = GetModuleHandleW(null());
            let class_name = wide(&format!(
                "WinremotePairingUI-{}-{:?}",
                std::process::id(),
                thread::current().id()
            ));
            let class = WNDCLASSW {
                lpfnWndProc: Some(wnd_proc),
                hInstance: instance,
                hCursor: LoadCursorW(null_mut(), IDC_ARROW),
                hbrBackground: (COLOR_WINDOW + 1) as *mut c_void,
                lpszClassName: class_name.as_ptr(),
                ..std::mem::zeroed()
            };
            if RegisterClassW(&class) == 0 {
                let _ = ready.send(Err(std::io::Error::last_os_error().to_string()));
                return;
            }
            let ready_events = events.clone();
            let state = Box::new(State {
                config,
                qr,
                events,
                shared: shared.clone(),
                invitation_edit: null_mut(),
                countdown: null_mut(),
                receive: null_mut(),
                request_text: null_mut(),
                code_text: null_mut(),
                privilege_text: null_mut(),
                confirm: null_mut(),
                approve: null_mut(),
                deny: null_mut(),
                prompts: Vec::new(),
                active_id: None,
                closing: false,
            });
            let state_ptr = Box::into_raw(state);
            let screen_w = GetSystemMetrics(SM_CXSCREEN);
            let screen_h = GetSystemMetrics(SM_CYSCREEN);
            let width = 760.min((screen_w - 20).max(500));
            let height = 750.min((screen_h - 20).max(600));
            let title = wide("Winremote MCP - connessione");
            let hwnd = CreateWindowExW(
                0,
                class_name.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
                10,
                10,
                width,
                height,
                null_mut(),
                null_mut(),
                instance,
                state_ptr.cast(),
            );
            if hwnd.is_null() {
                // WM_NCCREATE did not transfer ownership if creation failed early.
                // Once WM_NCCREATE succeeds, WM_NCDESTROY owns the Box.
                if !shared.transferred.load(Ordering::Acquire) {
                    drop(Box::from_raw(state_ptr));
                }
                let _ = ready.send(Err(std::io::Error::last_os_error().to_string()));
                UnregisterClassW(class_name.as_ptr(), instance);
                return;
            }
            {
                let mut mailbox = shared.inner.lock().unwrap_or_else(|e| e.into_inner());
                mailbox.hwnd = Some(hwnd as isize);
            }
            ShowWindow(hwnd, SW_SHOW);
            UpdateWindow(hwnd);
            if shared
                .inner
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .hwnd
                .is_none()
            {
                let _ = ready.send(Err("window closed during startup".into()));
                UnregisterClassW(class_name.as_ptr(), instance);
                return;
            }
            ready_events.send(UiEvent::SetAccessEnabled(true)).ok();
            let _ = ready.send(Ok(()));
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            UnregisterClassW(class_name.as_ptr(), instance);
        }
    }

    unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        if msg == WM_NCCREATE {
            let create = &*(l as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
            let state = &*(create.lpCreateParams as *mut State);
            state.shared.transferred.store(true, Ordering::Release);
            return 1;
        }
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, msg, w, l);
        }
        match msg {
            WM_CREATE => {
                create_controls(hwnd, &mut *ptr);
                SetTimer(hwnd, TIMER, 1000, None);
                0
            }
            WM_COMMAND => {
                let id = w & 0xffff;
                if id == ID_COPY || id == ID_SAVE {
                    // A native modal dialog can dispatch timers/close recursively. Do not
                    // keep any State borrow alive while its message loop is running.
                    let invitation = (&*ptr).config.invitation.clone();
                    let result = if id == ID_COPY {
                        copy_invitation(hwnd, &invitation)
                    } else {
                        save_png(hwnd, &invitation)
                    };
                    if let Err(error) = result {
                        if IsWindow(hwnd) != 0 {
                            show_error(hwnd, &error.to_string());
                        }
                    }
                } else {
                    let close = command(hwnd, &mut *ptr, id);
                    if close {
                        DestroyWindow(hwnd);
                    }
                }
                0
            }
            WM_TIMER => {
                let expired = {
                    let state = &mut *ptr;
                    if now() >= state.config.expires_unix {
                        state.events.send(UiEvent::Stop).ok();
                        state.events.send(UiEvent::SetAccessEnabled(false)).ok();
                        state.closing = true;
                        true
                    } else {
                        refresh(hwnd, state);
                        false
                    }
                };
                if expired {
                    DestroyWindow(hwnd);
                }
                0
            }
            WAKE => {
                let close = {
                    let state = &mut *ptr;
                    let commands = {
                        let mut mailbox =
                            state.shared.inner.lock().unwrap_or_else(|e| e.into_inner());
                        mailbox.queue.drain(..).collect::<Vec<_>>()
                    };
                    for command in commands {
                        match command {
                            Command::Requests(prompts) if !state.closing => {
                                state.prompts = prompts;
                                refresh(hwnd, state);
                            }
                            Command::Close => {
                                state.closing = true;
                                break;
                            }
                            _ => {}
                        }
                    }
                    state.closing
                };
                if close {
                    DestroyWindow(hwnd);
                }
                0
            }
            WM_PAINT => {
                paint_qr(hwnd, &*ptr);
                0
            }
            WM_CLOSE => {
                {
                    let state = &mut *ptr;
                    let programmatic_close = state
                        .shared
                        .inner
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .closing;
                    if !state.closing && !programmatic_close {
                        state.events.send(UiEvent::Stop).ok();
                        state.events.send(UiEvent::SetAccessEnabled(false)).ok();
                        state.closing = true;
                    }
                }
                DestroyWindow(hwnd);
                0
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            WM_NCDESTROY => {
                {
                    let state = &mut *ptr;
                    let mut mailbox = state.shared.inner.lock().unwrap_or_else(|e| e.into_inner());
                    mailbox.hwnd = None;
                    mailbox.closing = true;
                    mailbox.queue.clear();
                }
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(ptr));
                DefWindowProcW(hwnd, msg, w, l)
            }
            _ => DefWindowProcW(hwnd, msg, w, l),
        }
    }

    unsafe fn create_controls(hwnd: HWND, state: &mut State) {
        let mut rect: RECT = std::mem::zeroed();
        GetClientRect(hwnd, &mut rect);
        let width = rect.right - rect.left;
        control(
            hwnd,
            "STATIC",
            "Invito per la sessione corrente",
            WS_TABSTOP,
            0,
            18,
            346,
            width - 36,
            22,
            0,
        );
        state.invitation_edit = control(
            hwnd,
            "EDIT",
            &state.config.invitation,
            ES_AUTOHSCROLL as u32 | ES_READONLY as u32 | WS_TABSTOP,
            WS_EX_CLIENTEDGE,
            18,
            370,
            width - 36,
            29,
            0,
        );
        state.countdown = control(hwnd, "STATIC", "", 0, 0, 18, 408, 220, 24, 0);
        control(
            hwnd,
            "BUTTON",
            "Copia invito",
            BS_PUSHBUTTON as u32 | WS_TABSTOP,
            0,
            250,
            405,
            125,
            31,
            ID_COPY,
        );
        control(
            hwnd,
            "BUTTON",
            "Salva PNG...",
            BS_PUSHBUTTON as u32 | WS_TABSTOP,
            0,
            385,
            405,
            125,
            31,
            ID_SAVE,
        );
        control(
            hwnd,
            "BUTTON",
            "Arresta bridge",
            BS_PUSHBUTTON as u32 | WS_TABSTOP,
            0,
            520,
            405,
            145,
            31,
            ID_STOP,
        );
        state.receive = control(
            hwnd,
            "BUTTON",
            "Ricevi richieste di accesso",
            BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
            0,
            18,
            457,
            width - 36,
            29,
            ID_RECEIVE,
        );
        SendMessageW(state.receive, BM_SETCHECK, BST_CHECKED as usize, 0);
        state.request_text = control(
            hwnd,
            "STATIC",
            "Nessuna richiesta in attesa",
            0,
            0,
            18,
            494,
            width - 36,
            44,
            0,
        );
        state.code_text = control(hwnd, "STATIC", "", 0, 0, 18, 543, width - 36, 27, 0);
        state.privilege_text = control(hwnd, "STATIC", "", 0, 0, 18, 571, width - 36, 43, 0);
        state.confirm = control(
            hwnd,
            "BUTTON",
            "Il codice corrisponde a quello mostrato dal mio agente",
            BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
            0,
            18,
            620,
            width - 36,
            29,
            ID_CONFIRM,
        );
        state.approve = control(
            hwnd,
            "BUTTON",
            "Approva",
            BS_PUSHBUTTON as u32 | WS_TABSTOP,
            0,
            18,
            650,
            130,
            31,
            ID_APPROVE,
        );
        state.deny = control(
            hwnd,
            "BUTTON",
            "Rifiuta",
            BS_PUSHBUTTON as u32 | WS_TABSTOP,
            0,
            158,
            650,
            130,
            31,
            ID_DENY,
        );
        refresh(hwnd, state);
    }

    unsafe fn refresh(_hwnd: HWND, state: &mut State) {
        let remaining = state.config.expires_unix.saturating_sub(now());
        set_text(
            state.countdown,
            &format!(
                "Invito valido ancora {}:{:02}",
                remaining / 60,
                remaining % 60
            ),
        );
        let old_id = state.active_id.as_deref();
        let next = state.prompts.iter().find(|p| p.expires_unix > now());
        if next.map(|p| p.id.as_str()) != old_id {
            state.active_id = next.map(|p| p.id.clone());
            SendMessageW(state.confirm, BM_SETCHECK, BST_UNCHECKED as usize, 0);
        }
        if let Some(prompt) = next {
            let peer = display_text(&prompt.peer, 80);
            let label = display_text(&prompt.label, 64);
            let count = state
                .prompts
                .iter()
                .filter(|p| p.expires_unix > now())
                .count();
            set_text(state.request_text, &format!(
                "Richiesta da IP osservato: {peer}\r\nNome dichiarato (non verificato): {label}  -  {count} in coda"));
            set_text(
                state.code_text,
                &format!(
                    "Codice da confrontare: {}  ({} s)",
                    display_text(&prompt.code, 32),
                    prompt.expires_unix.saturating_sub(now())
                ),
            );
            set_text(
                state.privilege_text,
                if prompt.elevated {
                    "Accesso con privilegi amministrativi del bridge: comandi e file; screenshot e input se disponibili."
                } else {
                    "Accesso con i privilegi del bridge: comandi e file; screenshot e input se disponibili."
                },
            );
        } else {
            set_text(state.request_text, "Nessuna richiesta in attesa");
            set_text(state.code_text, "");
            set_text(state.privilege_text, "");
        }
        let shown = next.is_some();
        for control in [state.confirm, state.approve, state.deny] {
            ShowWindow(control, if shown { SW_SHOW } else { SW_HIDE });
        }
        let checked = SendMessageW(state.confirm, BM_GETCHECK, 0, 0) as u32 == BST_CHECKED;
        EnableWindow(state.approve, i32::from(shown && checked));
    }

    unsafe fn command(hwnd: HWND, state: &mut State, id: usize) -> bool {
        match id {
            ID_STOP => {
                state.events.send(UiEvent::Stop).ok();
                state.events.send(UiEvent::SetAccessEnabled(false)).ok();
                state.closing = true;
                return true;
            }
            ID_RECEIVE => {
                let checked = SendMessageW(state.receive, BM_GETCHECK, 0, 0) as u32 == BST_CHECKED;
                state.events.send(UiEvent::SetAccessEnabled(checked)).ok();
                if !checked {
                    state.prompts.clear();
                    refresh(hwnd, state);
                }
            }
            ID_CONFIRM => refresh(hwnd, state),
            ID_APPROVE | ID_DENY => {
                let checked = SendMessageW(state.confirm, BM_GETCHECK, 0, 0) as u32 == BST_CHECKED;
                if id == ID_APPROVE && !checked {
                    return false;
                }
                if let Some(request_id) = state.active_id.take() {
                    if !state
                        .prompts
                        .iter()
                        .any(|p| p.id == request_id && p.expires_unix > now())
                    {
                        refresh(hwnd, state);
                        return false;
                    }
                    state
                        .events
                        .send(UiEvent::Decide {
                            request_id: request_id.clone(),
                            approve: id == ID_APPROVE,
                        })
                        .ok();
                    state.prompts.retain(|p| p.id != request_id);
                    refresh(hwnd, state);
                }
            }
            _ => {}
        }
        false
    }

    unsafe fn paint_qr(hwnd: HWND, state: &State) {
        let mut paint: PAINTSTRUCT = std::mem::zeroed();
        let dc = BeginPaint(hwnd, &mut paint);
        if dc.is_null() {
            return;
        }
        let mut client: RECT = std::mem::zeroed();
        GetClientRect(hwnd, &mut client);
        let size = state.qr.width + 8;
        let scale = (320 / size).max(4) as i32;
        let edge = size as i32 * scale;
        let x = (client.right - edge) / 2;
        let y = 12;
        let white = CreateSolidBrush(0x00ffffff);
        let black = CreateSolidBrush(0);
        let area = RECT {
            left: x,
            top: y,
            right: x + edge,
            bottom: y + edge,
        };
        FillRect(dc, &area, white);
        for row in 0..state.qr.width {
            for col in 0..state.qr.width {
                if state.qr.modules[row * state.qr.width + col] {
                    let left = x + (col as i32 + 4) * scale;
                    let top = y + (row as i32 + 4) * scale;
                    let cell = RECT {
                        left,
                        top,
                        right: left + scale,
                        bottom: top + scale,
                    };
                    FillRect(dc, &cell, black);
                }
            }
        }
        DeleteObject(white);
        DeleteObject(black);
        EndPaint(hwnd, &paint);
    }

    unsafe fn copy_invitation(hwnd: HWND, invitation: &str) -> Result<()> {
        let text = wide(invitation);
        let bytes = text
            .len()
            .checked_mul(2)
            .ok_or_else(|| anyhow::anyhow!("Invito troppo lungo"))?;
        if OpenClipboard(hwnd) == 0 {
            bail!("clipboard occupata");
        }
        let result = (|| -> Result<()> {
            let memory = GlobalAlloc(GMEM_MOVEABLE, bytes);
            if memory.is_null() {
                bail!("memoria clipboard non disponibile");
            }
            let target = GlobalLock(memory);
            if target.is_null() {
                GlobalFree(memory);
                bail!("memoria clipboard non accessibile");
            }
            std::ptr::copy_nonoverlapping(text.as_ptr() as *const u8, target as *mut u8, bytes);
            GlobalUnlock(memory);
            if EmptyClipboard() == 0 {
                GlobalFree(memory);
                bail!("impossibile svuotare la clipboard");
            }
            if SetClipboardData(CF_UNICODETEXT, memory).is_null() {
                GlobalFree(memory);
                bail!("impossibile impostare la clipboard");
            }
            Ok(())
        })();
        CloseClipboard();
        result
    }

    unsafe fn save_png(hwnd: HWND, invitation: &str) -> Result<()> {
        let mut filename = [0u16; 32768];
        let proposal = wide("winremote-invite.png");
        filename[..proposal.len()].copy_from_slice(&proposal);
        let filter = wide("PNG (*.png)\0*.png\0All files (*.*)\0*.*\0");
        let title = wide("Salva il QR dell'invito");
        let extension = wide("png");
        let mut dialog: OPENFILENAMEW = std::mem::zeroed();
        dialog.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
        dialog.hwndOwner = hwnd;
        dialog.lpstrFilter = filter.as_ptr();
        dialog.nFilterIndex = 1;
        dialog.lpstrFile = filename.as_mut_ptr();
        dialog.nMaxFile = filename.len() as u32;
        dialog.lpstrTitle = title.as_ptr();
        dialog.lpstrDefExt = extension.as_ptr();
        dialog.Flags = OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR;
        if GetSaveFileNameW(&mut dialog) == 0 {
            let code = CommDlgExtendedError();
            if code == 0 {
                return Ok(());
            }
            bail!("dialogo di salvataggio non disponibile (codice {code})");
        }
        if IsWindow(hwnd) == 0 {
            return Ok(());
        }
        let len = filename
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(filename.len());
        let path = PathBuf::from(std::ffi::OsString::from_wide(&filename[..len]));
        let bytes = render_png(invitation)?;
        // The dialog already asked about overwriting; the common helper applies private ACLs.
        crate::connection::save_private_file(&path, &bytes, true)
    }

    unsafe fn show_error(hwnd: HWND, message: &str) {
        let message = wide(message);
        let title = wide("Winremote MCP");
        MessageBoxW(hwnd, message.as_ptr(), title.as_ptr(), MB_ICONERROR | MB_OK);
    }
}

#[cfg(windows)]
pub use win::{start, UiHandle};
