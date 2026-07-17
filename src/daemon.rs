use std::{
    fs,
    io::{BufRead, BufReader, Seek, Write},
    os::{
        fd::{AsFd, AsRawFd},
        unix::net::UnixListener,
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context, Result};
use chrono::Local;
use signal_hook::consts::{SIGINT, SIGTERM};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, delegate_noop,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_output, wl_registry},
};
use wayland_protocols_wlr::gamma_control::v1::client::{
    zwlr_gamma_control_manager_v1::ZwlrGammaControlManagerV1,
    zwlr_gamma_control_v1::{self, ZwlrGammaControlV1},
};

use crate::{
    config::{ConfigStore, Levels, Settings},
    conflict::{competing_gamma_processes, conflict_message},
    gamma::gamma_ramps,
    ipc::{prepare_runtime_dir, socket_path},
    protocol::{IPC_VERSION, Request, Response, RuntimeState},
    schedule::current_levels,
};

struct Output {
    global_name: u32,
    proxy: wl_output::WlOutput,
    name: String,
    gamma: ZwlrGammaControlV1,
    gamma_size: Option<u32>,
    failed: bool,
}

impl Output {
    fn display_name(&self) -> String {
        if self.failed {
            format!("{} (gamma control unavailable)", self.name)
        } else {
            self.name.clone()
        }
    }
}

struct DaemonState {
    manager: ZwlrGammaControlManagerV1,
    outputs: Vec<Output>,
    settings: Settings,
    store: ConfigStore,
    active: Levels,
    conflict: Option<String>,
}

impl DaemonState {
    fn output_mut(&mut self, global_name: u32) -> Option<&mut Output> {
        self.outputs
            .iter_mut()
            .find(|output| output.global_name == global_name)
    }

    fn refresh_conflict(&mut self) {
        let failed: Vec<String> = self
            .outputs
            .iter()
            .filter(|output| output.failed)
            .map(|output| output.name.clone())
            .collect();
        let competitors = competing_gamma_processes();
        self.conflict = conflict_message(&failed, &competitors);
    }

    fn add_output(
        &mut self,
        registry: &wl_registry::WlRegistry,
        qh: &QueueHandle<Self>,
        global_name: u32,
        version: u32,
    ) {
        if self
            .outputs
            .iter()
            .any(|output| output.global_name == global_name)
        {
            return;
        }
        let proxy = registry.bind(global_name, version.min(4), qh, global_name);
        let gamma = self.manager.get_gamma_control(&proxy, qh, global_name);
        self.outputs.push(Output {
            global_name,
            proxy,
            name: format!("output-{global_name}"),
            gamma,
            gamma_size: None,
            failed: false,
        });
    }

    fn remove_output(&mut self, global_name: u32) {
        if let Some(index) = self
            .outputs
            .iter()
            .position(|output| output.global_name == global_name)
        {
            let output = self.outputs.remove(index);
            output.gamma.destroy();
            if output.proxy.version() >= 3 {
                output.proxy.release();
            }
        }
    }

    fn refresh_active(&mut self) -> Result<()> {
        self.refresh_conflict();
        let next = current_levels(&self.settings, Local::now())?;
        if next != self.active {
            self.active = next;
            self.apply_all()?;
        }
        Ok(())
    }

    fn apply_all(&mut self) -> Result<()> {
        let ids: Vec<u32> = self
            .outputs
            .iter()
            .map(|output| output.global_name)
            .collect();
        for id in ids {
            self.apply_output(id)?;
        }
        Ok(())
    }

    fn apply_output(&mut self, global_name: u32) -> Result<()> {
        let active = self.active;
        let Some(output) = self.output_mut(global_name) else {
            return Ok(());
        };
        let Some(size) = output.gamma_size else {
            return Ok(());
        };
        if output.failed {
            return Ok(());
        }

        let ramps = gamma_ramps(size, active);
        let mut file = tempfile::tempfile()?;
        for value in ramps {
            file.write_all(&value.to_ne_bytes())?;
        }
        file.rewind()?;
        output.gamma.set_gamma(file.as_fd());
        Ok(())
    }

    fn runtime_state(&self) -> RuntimeState {
        RuntimeState {
            settings: self.settings.clone(),
            outputs: self.outputs.iter().map(Output::display_name).collect(),
            backend: "wlr-gamma-control-v1".into(),
            active_warmth: self.active.warmth,
            active_brightness: self.active.brightness,
            conflict: self.conflict.clone(),
        }
    }
}

pub fn run() -> Result<()> {
    let terminating = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(SIGTERM, terminating.clone())?;
    signal_hook::flag::register(SIGINT, terminating.clone())?;
    run_until(terminating)
}

pub struct TransientBackend {
    terminating: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<()>>>,
}

impl TransientBackend {
    pub fn start() -> Self {
        let terminating = Arc::new(AtomicBool::new(false));
        let thread_terminating = terminating.clone();
        let thread = thread::spawn(move || run_until(thread_terminating));
        Self {
            terminating,
            thread: Some(thread),
        }
    }

    pub fn check_running(&mut self) -> Result<()> {
        let Some(thread) = &self.thread else {
            anyhow::bail!("temporary display backend stopped unexpectedly");
        };
        if !thread.is_finished() {
            return Ok(());
        }

        let result = self.thread.take().unwrap().join();
        match result {
            Ok(Ok(())) => anyhow::bail!("temporary display backend stopped unexpectedly"),
            Ok(Err(error)) => Err(error).context("temporary display backend failed"),
            Err(_) => anyhow::bail!("temporary display backend panicked"),
        }
    }
}

impl Drop for TransientBackend {
    fn drop(&mut self) {
        self.terminating.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_until(terminating: Arc<AtomicBool>) -> Result<()> {
    let store = ConfigStore::discover()?;
    let settings = store.load_or_create()?;
    let active = current_levels(&settings, Local::now())?;
    let (listener, _socket_guard) = bind_listener()?;

    let connection = Connection::connect_to_env().context(
        "failed to connect to Wayland; ensure WAYLAND_DISPLAY is imported into the user service",
    )?;
    let (globals, mut event_queue) = registry_queue_init::<DaemonState>(&connection)?;
    let qh = event_queue.handle();
    let manager: ZwlrGammaControlManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .context("this compositor does not support wlr-gamma-control; Waywarm supports wlroots compositors only")?;

    let mut state = DaemonState {
        manager,
        outputs: Vec::new(),
        settings,
        store,
        active,
        conflict: None,
    };
    for global in globals.contents().clone_list() {
        if global.interface == wl_output::WlOutput::interface().name {
            state.add_output(globals.registry(), &qh, global.name, global.version);
        }
    }
    event_queue.roundtrip(&mut state)?;
    state.refresh_conflict();
    state.apply_all()?;

    while !terminating.load(Ordering::Relaxed) {
        event_queue.dispatch_pending(&mut state)?;
        state.refresh_active()?;
        connection.flush()?;

        let guard = connection.prepare_read();
        let wayland_fd = connection.backend().poll_fd().as_raw_fd();
        let mut fds = [
            libc::pollfd {
                fd: wayland_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: both descriptors remain valid for the duration of poll.
        let result = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 250) };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error.into());
            }
        }
        if fds[0].revents & libc::POLLIN != 0 {
            if let Some(guard) = guard {
                guard.read()?;
            }
            event_queue.dispatch_pending(&mut state)?;
        } else {
            drop(guard);
        }
        if fds[1].revents & libc::POLLIN != 0 {
            accept_clients(&listener, &mut state);
        }
    }

    for output in state.outputs.drain(..) {
        output.gamma.destroy();
    }
    connection.flush()?;
    Ok(())
}

fn bind_listener() -> Result<(UnixListener, SocketGuard)> {
    prepare_runtime_dir()?;
    let path = socket_path()?;
    remove_stale_socket(&path)?;

    let listener = UnixListener::bind(&path)?;
    listener.set_nonblocking(true)?;
    fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    Ok((listener, SocketGuard(path)))
}

fn remove_stale_socket(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }

    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => anyhow::bail!("another Waywarm daemon is already running"),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            ) =>
        {
            fs::remove_file(path)
                .with_context(|| format!("failed to remove stale {}", path.display()))
        }
        Err(error) => Err(error).context("failed to inspect the existing daemon socket"),
    }
}

fn accept_clients(listener: &UnixListener, state: &mut DaemonState) {
    while let Ok((stream, _)) = listener.accept() {
        if let Err(error) = handle_client(stream, state) {
            eprintln!("waywarm: IPC request failed: {error:#}");
        }
    }
}

fn handle_client(
    mut stream: std::os::unix::net::UnixStream,
    state: &mut DaemonState,
) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    let mut line = String::new();
    BufReader::new(stream.try_clone()?).read_line(&mut line)?;
    let response = match serde_json::from_str(&line) {
        Ok(request) => handle_request(request, state),
        Err(error) => error_response(format!("invalid request: {error}")),
    };
    serde_json::to_writer(&mut stream, &response)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
}

fn handle_request(request: Request, state: &mut DaemonState) -> Response {
    match request {
        Request::GetState { version } if version == IPC_VERSION => state_response(state),
        Request::ReplaceSettings { version, settings } if version == IPC_VERSION => {
            match update_settings(state, settings) {
                Ok(()) => state_response(state),
                Err(error) => error_response(format!("{error:#}")),
            }
        }
        Request::GetState { version } | Request::ReplaceSettings { version, .. } => {
            error_response(format!(
                "unsupported IPC protocol version {version} (daemon expects {IPC_VERSION}); reinstall and restart the Waywarm service"
            ))
        }
    }
}

fn state_response(state: &DaemonState) -> Response {
    Response::State {
        version: IPC_VERSION,
        state: state.runtime_state(),
    }
}

fn error_response(message: impl Into<String>) -> Response {
    Response::Error {
        version: IPC_VERSION,
        message: message.into(),
    }
}

fn update_settings(state: &mut DaemonState, settings: Settings) -> Result<()> {
    settings.validate()?;
    state.store.save(&settings)?;
    state.settings = settings;
    state.active = current_levels(&state.settings, Local::now())?;
    state.apply_all()
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for DaemonState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _data: &GlobalListContents,
        _connection: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } if interface == wl_output::WlOutput::interface().name => {
                state.add_output(registry, qh, name, version);
            }
            wl_registry::Event::GlobalRemove { name } => state.remove_output(name),
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, u32> for DaemonState {
    fn event(
        state: &mut Self,
        _proxy: &wl_output::WlOutput,
        event: wl_output::Event,
        global_name: &u32,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event
            && let Some(output) = state.output_mut(*global_name)
        {
            output.name = name;
        }
    }
}

impl Dispatch<ZwlrGammaControlV1, u32> for DaemonState {
    fn event(
        state: &mut Self,
        _proxy: &ZwlrGammaControlV1,
        event: zwlr_gamma_control_v1::Event,
        global_name: &u32,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_gamma_control_v1::Event::GammaSize { size } => {
                if let Some(output) = state.output_mut(*global_name) {
                    output.gamma_size = Some(size);
                }
                if let Err(error) = state.apply_output(*global_name) {
                    eprintln!("waywarm: failed to apply gamma ramp: {error:#}");
                }
            }
            zwlr_gamma_control_v1::Event::Failed => {
                if let Some(output) = state.output_mut(*global_name) {
                    output.failed = true;
                }
                state.refresh_conflict();
            }
            _ => {}
        }
    }
}

delegate_noop!(DaemonState: ignore ZwlrGammaControlManagerV1);

struct SocketGuard(PathBuf);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
