use std::{
    env, fs,
    io::{BufRead, BufReader, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::PathBuf,
    time::Duration,
};

use anyhow::{Context, Result, bail};

use crate::protocol::{IPC_VERSION, Request, Response, RuntimeState};

pub fn runtime_dir() -> Result<PathBuf> {
    let base = env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
    Ok(PathBuf::from(base).join("waywarm"))
}

pub fn socket_path() -> Result<PathBuf> {
    Ok(runtime_dir()?.join("control.sock"))
}

pub fn prepare_runtime_dir() -> Result<PathBuf> {
    let directory = runtime_dir()?;
    fs::create_dir_all(&directory)?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    Ok(directory)
}

pub fn query_state() -> Result<RuntimeState> {
    request_state(Request::GetState {
        version: IPC_VERSION,
    })
}

pub fn replace_settings(settings: crate::config::Settings) -> Result<RuntimeState> {
    request_state(Request::ReplaceSettings {
        version: IPC_VERSION,
        settings,
    })
}

fn request_state(request: Request) -> Result<RuntimeState> {
    match exchange(&request)? {
        Response::State { state, .. } => Ok(state),
        Response::Error { message, .. } => bail!(message),
    }
}

fn exchange(request: &Request) -> Result<Response> {
    let path = socket_path()?;
    let mut stream = UnixStream::connect(&path).with_context(|| {
        format!(
            "cannot connect to the Waywarm daemon at {}; start waywarm.service",
            path.display()
        )
    })?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    serde_json::to_writer(&mut stream, request)?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    if line.is_empty() {
        bail!("daemon closed the connection without a response");
    }
    Ok(serde_json::from_str(&line)?)
}
