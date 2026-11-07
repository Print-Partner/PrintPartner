use anyhow::{Context, Result, ensure};
use pp_core::{CoreHandle, CoreRuntime, RuntimeStatus};
use pp_desktop::{AllowedOrigin, ResourceLayout, desktop_data_dir, startup_diagnostic};
use std::path::PathBuf;
use tauri::{
    Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent,
    image::Image,
    menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem},
    tray::TrayIconBuilder,
    webview::{DownloadEvent, NewWindowResponse},
};
mod recovery;

#[cfg(all(not(debug_assertions), feature = "desktop-e2e"))]
compile_error!("the desktop-e2e feature is not part of a release build");

#[cfg(feature = "desktop-e2e")]
static AUTOMATION_BUILD_MARKER: &str = "pp-desktop-e2e-marker-gre492";

#[cfg(feature = "desktop-e2e")]
fn automation_marker() -> &'static str {
    AUTOMATION_BUILD_MARKER
}

fn main() -> std::process::ExitCode {
    // Hold tracing rows and arm the crash hook before anything can log; rows
    // reach core.log once core startup admits the data directory. A failure
    // before that point is reported on stderr only.
    let core_log = pp_core::CoreLog::buffered();
    if !core_log.install() {
        eprintln!("core_log_unavailable: subscriber_already_installed");
    }
    match run(&core_log) {
        Ok(code) => std::process::ExitCode::from(code),
        Err(error) => {
            let diagnostic = startup_diagnostic(&error);
            tracing::error!("{diagnostic}");
            eprintln!("{diagnostic}");
            if cfg!(debug_assertions)
                && std::env::args().any(|argument| argument == "--test-exit-seconds")
            {
                return std::process::ExitCode::FAILURE;
            }
            show_startup_error();
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(feature = "desktop_rebuild")]
fn show_origin_notice(text: &str) {
    #[cfg(target_os = "linux")]
    {
        use gtk::prelude::*;
        let dialog = gtk::MessageDialog::builder()
            .title("Print Partner")
            .text(text)
            .message_type(gtk::MessageType::Info)
            .buttons(gtk::ButtonsType::Ok)
            .modal(true)
            .build();
        dialog.run();
        dialog.close();
    }
    #[cfg(not(target_os = "linux"))]
    {
        rfd::MessageDialog::new()
            .set_title("Print Partner")
            .set_description(text)
            .set_level(rfd::MessageLevel::Info)
            .show();
    }
}

fn show_startup_error() {
    let title = "Print Partner could not start";
    let description = "The desktop runtime or its verified resources could not start. Check that this installation is complete and that another Print Partner instance is not using its data directory.";
    #[cfg(target_os = "linux")]
    {
        use gtk::prelude::*;
        // Tauri initializes GTK on this thread and owns its main context. A
        // background dialog thread cannot acquire that context before run().
        // Run the modal GTK loop here, including failures before shell setup.
        if gtk::init().is_ok() {
            let dialog = gtk::MessageDialog::builder()
                .title(title)
                .text(title)
                .secondary_text(description)
                .message_type(gtk::MessageType::Error)
                .buttons(gtk::ButtonsType::Ok)
                .modal(true)
                .build();
            dialog.run();
            dialog.close();
        }
    }
    #[cfg(not(target_os = "linux"))]
    rfd::MessageDialog::new()
        .set_title(title)
        .set_description(description)
        .set_level(rfd::MessageLevel::Error)
        .show();
}

fn run(core_log: &pp_core::CoreLog) -> Result<u8> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    tauri::async_runtime::set(runtime.handle().clone());
    #[cfg(feature = "desktop-e2e")]
    let builder = tauri::Builder::default().plugin(tauri_plugin_wdio_webdriver::init());
    #[cfg(not(feature = "desktop-e2e"))]
    let builder = tauri::Builder::default();
    #[cfg(feature = "desktop-e2e")]
    if std::env::var_os("PP_DESKTOP_E2E_MARKER").is_some() {
        println!("{}", automation_marker());
    }
    #[cfg(feature = "desktop_rebuild")]
    let builder = builder.invoke_handler(tauri::generate_handler![
        pp_desktop::native_actions::open_sponsor,
        pp_desktop::origin_change::sync_portable_settings
    ]);
    let app = builder
        .build(tauri::generate_context!())
        .context("Native shell initialization failed")?;
    let launch_args =
        pp_desktop::LaunchArgs::parse(std::env::args().skip(1), cfg!(debug_assertions))?;
    // The login LaunchAgent passes --minimized; the tray's Show brings the window back.
    let window_visible = launch_args.window_visible();
    let pp_desktop::LaunchArgs {
        stage,
        data,
        test_exit,
        test_recovery_logs,
        ..
    } = launch_args;
    let stage = match stage {
        Some(stage) => stage,
        None => {
            let resources = app.path().resource_dir()?;
            if cfg!(target_os = "macos") {
                resources
                    .parent()
                    .context("App Contents unavailable")?
                    .to_owned()
            } else {
                resources.join("desktop-runtime")
            }
        }
    };
    let data = desktop_data_dir(data, || {
        app.path()
            .home_dir()
            .context("Desktop home directory unavailable")
    })?;
    let launch = ResourceLayout::read(&stage)
        .and_then(|layout| layout.into_launch(data.clone()))
        .context("Desktop resource verification failed")?;
    let probe_diagnostics = recovery::RecoveryUi::new(data.clone());
    let mut core = match runtime.block_on(CoreRuntime::start_for_launcher(launch)) {
        Ok(core) => core,
        Err(error) => {
            let error = error.context("Desktop core startup failed");
            // T-30: another desktop holds this data directory. Show its window
            // instead of an error when it confirms the same directory.
            if pp_desktop::focus::owned_by_another_desktop(&error)
                && pp_desktop::focus::request(&data).unwrap_or(false)
            {
                eprintln!("desktop_already_running: focused_existing_window");
                return Ok(0);
            }
            return Err(error);
        }
    };
    let focus_data = data.clone();
    // Core startup admitted the data directory; flush the held rows and
    // record tracing and panics in its logs folder, telemetry or not.
    if let Err(error) = core_log.attach(&data) {
        eprintln!(
            "core_log_unavailable: os_error={}",
            error.raw_os_error().unwrap_or_default()
        );
    }
    let setup = (|| -> Result<_> {
        let origin = AllowedOrigin::parse(core.origin())?;
        let browser_origin = origin.clone();
        let launch_url = core.take_launch_target()?.into_url();
        #[cfg(feature = "desktop_rebuild")]
        let (sponsor, origin_bridge) = {
            let enabled = core.desktop_rebuild().enabled();
            let sponsor = pp_desktop::native_actions::SponsorAction::from_startup(
                core.desktop_rebuild(),
                origin.clone(),
            );
            let origin_bridge = if enabled {
                pp_desktop::origin_change::require_fresh_bootstrap(
                    core.origin_change(),
                    &launch_url,
                )
                .context("Desktop core startup failed")?;
                let preferences = match runtime.block_on(core.handle().browser_preferences()) {
                    Ok(preferences) => Some(preferences),
                    Err(error) => {
                        eprintln!("{}", pp_desktop::browser_preferences_diagnostic(&error));
                        None
                    }
                };
                pp_desktop::origin_change::OriginBridge::enabled(
                    origin.clone(),
                    core.handle(),
                    preferences.as_ref(),
                    core.origin_change(),
                )
            } else {
                pp_desktop::origin_change::OriginBridge::disabled()
            };
            (sponsor, origin_bridge)
        };
        #[cfg(feature = "desktop_rebuild")]
        let origin_notice = origin_bridge.notice().map(str::to_owned);
        #[cfg(not(feature = "desktop_rebuild"))]
        let origin_notice: Option<String> = None;
        let url = launch_url.parse()?;
        #[cfg(feature = "desktop_rebuild")]
        if let Some(capability) = sponsor.capability() {
            app.add_capability(capability)?;
        }
        #[cfg(feature = "desktop_rebuild")]
        if let Some(capability) = origin_bridge.capability() {
            app.add_capability(capability)?;
        }
        let downloads = pp_desktop::downloads::DownloadPolicy::new(
            origin.clone(),
            app.path().download_dir().ok(),
        );
        let builder = WebviewWindowBuilder::new(&app, "main", WebviewUrl::External(url));
        #[cfg(feature = "desktop_rebuild")]
        let builder = {
            let builder = if let Some(script) = sponsor.initialization_script() {
                builder.initialization_script(script)
            } else {
                builder
            };
            let builder = if let Some(script) = origin_bridge.initialization_script() {
                builder.initialization_script(script)
            } else {
                builder
            };
            app.manage(sponsor);
            app.manage(origin_bridge);
            builder
        };
        let window = builder
            .title("Print Partner")
            .visible(window_visible)
            .inner_size(1280.0, 850.0)
            .min_inner_size(900.0, 650.0)
            .on_navigation(move |url| origin.admits(url))
            .on_new_window(|_, _| NewWindowResponse::Deny)
            .on_download(move |webview, event| match event {
                DownloadEvent::Requested { url, destination } => {
                    match downloads.place(webview.label(), &url, destination) {
                        Ok(path) => {
                            *destination = path;
                            tracing::info!(scheme = url.scheme(), "download_admitted");
                            true
                        }
                        Err(denied) => {
                            tracing::warn!(scheme = url.scheme(), "{}", denied.code());
                            false
                        }
                    }
                }
                DownloadEvent::Finished { url, success, .. } => {
                    tracing::info!(scheme = url.scheme(), success, "download_finished");
                    true
                }
                _ => false,
            })
            .build()
            .context("Native window initialization failed")?;
        let hide = window.clone();
        window.on_window_event(move |event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = hide.hide();
            }
        });
        let watch = build_tray(&app, core.handle(), browser_origin, data)
            .context("Native tray initialization failed")?;
        Ok((window, watch, origin_notice))
    })();
    let (window, status_watch, origin_notice) = match setup {
        Ok(window) => window,
        Err(error) => {
            let receipt = runtime.block_on(core.shutdown());
            ensure!(receipt.complete(), "Desktop shutdown incomplete");
            return Err(error);
        }
    };
    let focus_app = app.handle().clone();
    let focus_server = {
        let _runtime = runtime.enter();
        pp_desktop::focus::FocusServer::start(&focus_data, move || {
            let app = focus_app.clone();
            let _ = focus_app.run_on_main_thread(move || {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.unminimize();
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            });
        })
    }
    .inspect_err(|_| eprintln!("desktop_focus_unavailable"))
    .ok();
    #[cfg(feature = "desktop_rebuild")]
    let origin_notice_shown = std::sync::atomic::AtomicBool::new(false);
    let shutdown_requested = core.shutdown_requested();
    let app_handle = app.handle().clone();
    let stop_signal = runtime.spawn(async move {
        let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
        tokio::select! {
            _ = shutdown_requested => {},
            _ = tokio::signal::ctrl_c() => {},
            _ = async { if let Some(signal) = terminate.as_mut() { signal.recv().await; } else { std::future::pending::<()>().await; } } => {},
            _ = async { if let Some(seconds) = test_exit { tokio::time::sleep(std::time::Duration::from_secs(seconds)).await; } else { std::future::pending::<()>().await; } } => {},
        }
        app_handle.exit(0);
    });
    let runtime_handle = runtime.handle().clone();
    let mut status_watch = Some(status_watch);
    let exit_code = app.run_return(move |_app, event| {
        // Finish the watch task before the app is torn down, so its app
        // handle is never the last one and nothing AppKit owns is dropped
        // on a tokio worker (GRE-1254).
        if matches!(event, tauri::RunEvent::Exit)
            && let Some(watch) = status_watch.take()
        {
            watch.abort();
            let _ = runtime_handle.block_on(watch);
        }
        #[cfg(feature = "desktop_rebuild")]
        if matches!(event, tauri::RunEvent::Ready)
            && !origin_notice_shown.swap(true, std::sync::atomic::Ordering::AcqRel)
            && let Some(notice) = origin_notice.as_deref()
        {
            show_origin_notice(notice);
        }
        #[cfg(not(feature = "desktop_rebuild"))]
        let _ = &origin_notice;
        if cfg!(debug_assertions) && test_exit.is_some() && matches!(event, tauri::RunEvent::Ready) {
            if test_recovery_logs {
                probe_diagnostics.view_logs(_app);
            }
            let window = window.clone();
            let app = _app.clone();
            let _ = window.close();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                let (send, receive) = tokio::sync::oneshot::channel();
                let show = window.clone();
                let _ = app.run_on_main_thread(move || {
                    let hidden = show.is_visible().is_ok_and(|visible| !visible);
                    let _ = show.show();
                    let _ = send.send(hidden);
                });
                let hidden = receive.await.unwrap_or(false);
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                let _ = app.run_on_main_thread(move || {
                    let shown = window.is_visible().is_ok_and(|visible| visible);
                    println!("{}", serde_json::json!({"native_window": {"close_hid": hidden, "show_visible": shown}}));
                });
            });
        }
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Reopen { .. } = event {
            let _ = window.show();
            let _ = window.set_focus();
        }
        let _ = (&window, &event);
    });
    stop_signal.abort();
    // Remove the focus socket while this process still holds the data lock.
    drop(focus_server);
    let receipt = runtime.block_on(core.shutdown());
    println!("{}", serde_json::to_string(&receipt)?);
    ensure!(receipt.complete(), "Desktop shutdown incomplete");
    Ok(u8::try_from(exit_code).unwrap_or(1))
}

const TRAY_ID: &str = "printpartner";

struct TrayStatusItems {
    status: MenuItem<tauri::Wry>,
    recover: MenuItem<tauri::Wry>,
}

fn build_tray(
    app: &tauri::App,
    handle: CoreHandle,
    origin: AllowedOrigin,
    data: PathBuf,
) -> Result<tauri::async_runtime::JoinHandle<()>> {
    let show = MenuItem::with_id(app, "show", "Show Print Partner", true, None::<&str>)?;
    let browser = MenuItem::with_id(app, "browser", "Open in browser", true, None::<&str>)?;
    let status = MenuItem::with_id(app, "status", "Service ready", false, None::<&str>)?;
    let recover = MenuItem::with_id(app, "recover", "Restart service", false, None::<&str>)?;
    let logs = MenuItem::with_id(app, "logs", "View logs", true, None::<&str>)?;
    let folder = MenuItem::with_id(app, "data", "Open data folder", true, None::<&str>)?;
    let log_folder = MenuItem::with_id(app, "log_folder", "Reveal log folder", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Print Partner", true, None::<&str>)?;
    // Open at login is a per-user LaunchAgent, so only macOS gets the item (T-31).
    let login_agent = if cfg!(target_os = "macos") {
        app.path()
            .home_dir()
            .ok()
            .zip(std::env::current_exe().ok())
            .and_then(|(home, exe)| pp_desktop::autostart::LaunchAgent::new(&home, &exe).ok())
    } else {
        None
    };
    let open_at_login = CheckMenuItem::with_id(
        app,
        pp_desktop::tray::OPEN_AT_LOGIN,
        "Open at login",
        true,
        login_agent.as_ref().is_some_and(|agent| agent.is_enabled()),
        None::<&str>,
    )?;
    let all: [&dyn IsMenuItem<tauri::Wry>; 9] = [
        &show,
        &browser,
        &status,
        &logs,
        &log_folder,
        &recover,
        &folder,
        &open_at_login,
        &quit,
    ];
    let ids = if login_agent.is_some() {
        pp_desktop::tray::menu_ids_with_open_at_login()
    } else {
        pp_desktop::tray::menu_ids().to_vec()
    };
    let items = ids
        .iter()
        .map(|id| {
            all.iter()
                .copied()
                .find(|item| item.id().as_ref() == *id)
                .with_context(|| format!("Tray menu item {id} missing"))
        })
        .collect::<Result<Vec<_>>>()?;
    let menu = Menu::with_items(app, &items)?;
    let status_receiver = handle.subscribe();
    let recovery = handle.clone();
    let browser_handle = handle.clone();
    let gate = std::sync::Arc::new(recovery::RecoveryGate::default());
    let recovery_gate = gate.clone();
    let diagnostics = recovery::RecoveryUi::new(data);
    let icon = app
        .default_window_icon()
        .context("Desktop icon missing")?
        .clone()
        .to_owned();
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon.clone())
        .tooltip("Print Partner")
        .menu(&menu)
        .on_menu_event(move |app, event| match event.id.as_ref() {
            "show" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            "browser" => recovery::open_in_browser(app, &browser_handle, &origin),
            "recover" => {
                recovery::restart(app, recovery.clone(), recovery_gate.clone());
            }
            "logs" => diagnostics.view_logs(app),
            "data" => diagnostics.open_data(app),
            "log_folder" => diagnostics.reveal_logs(app),
            "quit" => app.exit(0),
            id if id == pp_desktop::tray::OPEN_AT_LOGIN => {
                if let Some(agent) = &login_agent {
                    let on =
                        pp_desktop::tray::toggle_open_at_login(agent).unwrap_or_else(|error| {
                            tracing::error!("open_at_login_failed: {error:#}");
                            agent.is_enabled()
                        });
                    let _ = open_at_login.set_checked(on);
                }
            }
            _ => {}
        })
        .build(app)?;
    // AppKit objects must be dropped on the main thread. The watch task runs
    // on a tokio worker, so it holds only the app handle and looks the tray
    // and its status items up inside run_on_main_thread (GRE-1254).
    app.manage(TrayStatusItems { status, recover });
    let app_handle = app.handle().clone();
    let watch = tauri::async_runtime::spawn(async move {
        let mut receiver = status_receiver;
        let mut shown_tone = None;
        loop {
            let state = receiver.borrow_and_update().clone();
            let (text, tone) = pp_desktop::tray::runtime_status(&state);
            let badged = (shown_tone != Some(tone)).then(|| {
                Image::new_owned(
                    pp_desktop::tray::badge(icon.rgba(), icon.width(), icon.height(), tone),
                    icon.width(),
                    icon.height(),
                )
            });
            let guarded = match &state {
                RuntimeStatus::Node(state) => gate.observe(state),
                RuntimeStatus::Rust(_) => false,
            };
            let app = app_handle.clone();
            if app_handle
                .run_on_main_thread(move || {
                    if let Some(items) = app.try_state::<TrayStatusItems>() {
                        let _ = items.status.set_text(text);
                        let _ = items.recover.set_enabled(guarded);
                    }
                    if let (Some(badged), Some(tray)) = (badged, app.tray_by_id(TRAY_ID)) {
                        let _ = tray.set_icon(Some(badged));
                    }
                })
                .is_err()
            {
                break;
            }
            shown_tone = Some(tone);
            if receiver.changed().await.is_err() {
                break;
            }
        }
    });
    Ok(watch)
}

#[cfg(test)]
mod automation_boundary {
    #[test]
    fn desktop_e2e_is_opt_in() {
        const { assert!(!cfg!(feature = "desktop-e2e")) };
    }
}
