//! seekr-svc: Seekr's index service. Run `seekr-svc install` or `seekr-svc uninstall` as
//! administrator; the service control manager starts it as LocalSystem with `run`.

mod security;
mod server;
mod tree;

use std::{
    error::Error,
    ffi::OsString,
    path::PathBuf,
    process::ExitCode,
    sync::{Arc, atomic::Ordering, mpsc},
    time::{Duration, Instant},
};

use windows_service::{
    define_windows_service,
    service::{
        ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
        ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult, ServiceStatusHandle},
    service_dispatcher,
    service_manager::{ServiceManager, ServiceManagerAccess},
};

const NAME: &str = "SeekrIndex";
const DISPLAY_NAME: &str = "Seekr file index";
const DESCRIPTION: &str = "Reads NTFS file names for Seekr's instant file search and serves them \
                           only to the signed-in user, without other accounts' profiles.";

fn main() -> ExitCode {
    let result = match std::env::args().nth(1).as_deref() {
        Some("install") => install(),
        Some("uninstall") => uninstall(),
        Some("run") => service_dispatcher::start(NAME, ffi_service_main).map_err(Into::into),
        _ => Err("usage: seekr-svc install | uninstall (as administrator)".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("seekr-svc: {e}");
            ExitCode::FAILURE
        }
    }
}

define_windows_service!(ffi_service_main, service_main);

fn service_main(_arguments: Vec<OsString>) {
    // LocalSystem's %LOCALAPPDATA%\Seekr\logs, readable by local users for diagnosis.
    let logs = common::data_dir().join("logs");
    let shared = std::fs::create_dir_all(&logs).and_then(|()| security::share_logs(&logs));
    let _log = common::init_logging("seekr-svc");
    if let Err(e) = shared {
        tracing::warn!("logs stay private to administrators: {e}");
    }
    if let Err(e) = run() {
        tracing::error!("service stopped with an error: {e}");
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let control = Arc::new(server::Control::default());
    let (wake_tx, wake_rx) = mpsc::channel::<()>();
    let handler_control = control.clone();
    let status = service_control_handler::register(NAME, move |event| match event {
        ServiceControl::Stop => {
            handler_control.stop.store(true, Ordering::SeqCst);
            server::wake();
            let _ = wake_tx.send(());
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::SessionChange(_) => {
            handler_control
                .session_changed
                .store(true, Ordering::SeqCst);
            server::wake();
            let _ = wake_tx.send(());
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    })?;
    report(&status, ServiceState::Running)?;
    tracing::info!("running");

    while !control.stop.load(Ordering::SeqCst) {
        match security::console_user() {
            Some(sid) => {
                tracing::info!("serving {sid}");
                if let Err(e) = server::serve_user(&sid, &control) {
                    tracing::error!("serving {sid} failed: {e}; retrying in 10 s");
                    let _ = wake_rx.recv_timeout(Duration::from_secs(10));
                }
            }
            // Sign-in screen: wait for a session change.
            None => {
                let _ = wake_rx.recv_timeout(Duration::from_secs(30));
            }
        }
    }
    report(&status, ServiceState::Stopped)?;
    Ok(())
}

fn report(status: &ServiceStatusHandle, state: ServiceState) -> windows_service::Result<()> {
    status.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: if state == ServiceState::Running {
            ServiceControlAccept::STOP | ServiceControlAccept::SESSION_CHANGE
        } else {
            ServiceControlAccept::empty()
        },
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    })
}

/// A LocalSystem service must run from a folder only administrators can write; under
/// %LOCALAPPDATA% any program of the user could swap the exe and gain SYSTEM.
fn install_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("ProgramFiles").unwrap_or_else(|| r"C:\Program Files".into()))
        .join("Seekr")
}

fn install() -> Result<(), Box<dyn Error>> {
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )?;
    remove_existing(&manager)?;
    let dest = install_dir().join("seekr-svc.exe");
    std::fs::create_dir_all(install_dir())?;
    let source = std::env::current_exe()?;
    if source != dest {
        std::fs::copy(&source, &dest)?;
    }
    let info = ServiceInfo {
        name: NAME.into(),
        display_name: DISPLAY_NAME.into(),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: dest.clone(),
        launch_arguments: vec!["run".into()],
        dependencies: vec![],
        account_name: None,
        account_password: None,
    };
    // A just-deleted service lingers until its handles close; retry briefly.
    let deadline = Instant::now() + Duration::from_secs(10);
    let service = loop {
        match manager.create_service(&info, ServiceAccess::CHANGE_CONFIG | ServiceAccess::START) {
            Ok(s) => break s,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(500)),
            Err(e) => return Err(e.into()),
        }
    };
    service.set_description(DESCRIPTION)?;
    service.start::<&str>(&[])?;
    println!("installed {} and started {NAME}", dest.display());
    Ok(())
}

fn uninstall() -> Result<(), Box<dyn Error>> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    remove_existing(&manager)?;
    let _ = std::fs::remove_dir_all(install_dir());
    // The service's snapshots and logs live in LocalSystem's profile.
    let system_data =
        PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into()))
            .join(r"System32\config\systemprofile\AppData\Local\Seekr");
    let _ = std::fs::remove_dir_all(system_data);
    println!("removed {NAME}");
    Ok(())
}

/// Stops and deletes the service if it exists, waiting until its process has exited.
fn remove_existing(manager: &ServiceManager) -> Result<(), Box<dyn Error>> {
    let access = ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE;
    let Ok(service) = manager.open_service(NAME, access) else {
        return Ok(());
    };
    if service.query_status()?.current_state != ServiceState::Stopped {
        let _ = service.stop();
        let deadline = Instant::now() + Duration::from_secs(30);
        while service.query_status()?.current_state != ServiceState::Stopped {
            if Instant::now() > deadline {
                return Err(format!("{NAME} did not stop within 30 s").into());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    service.delete()?;
    Ok(())
}
