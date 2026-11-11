use crate::account::helpers::misc::get_selected_player_info;
use crate::account::helpers::offline::yggdrasil_server::YggdrasilServer;
use crate::account::helpers::{authlib_injector, microsoft};
use crate::account::models::PlayerType;
use crate::error::{USTBLError, USTBLResult};
use crate::instance::helpers::client_json::{replace_native_libraries, McClientInfo};
use crate::instance::helpers::identity::find_instance;
use crate::instance::helpers::misc::{get_instance_game_config, get_instance_subdir_paths};
use crate::instance::models::misc::{Instance, InstanceError, InstanceSubdirType, ModLoaderStatus};
use crate::instance::modpack_update::apply::UpdateGuard;
use crate::instance::modpack_update::commands::{has_campus_install, pending_campus_instances};
use crate::launch::helpers::command_generator::{
  export_full_launch_command, generate_launch_command, LaunchCommand,
};
use crate::launch::helpers::file_validator::{
  extract_native_libraries, get_invalid_assets, get_invalid_library_files, prepare_legacy_assets,
};
use crate::launch::helpers::jre_selector::{
  instance_requires_java_25, select_component_java, select_java_runtime,
};
use crate::launch::helpers::log_parser::parse_crash_report_path_from_log;
use crate::launch::helpers::misc::get_separator;
use crate::launch::helpers::process_monitor::{
  create_game_log, game_log_path, kill_process, monitor_process, set_process_priority,
};
use crate::launch::helpers::session::{
  cancel_session, next_launching_id, register_game_process, remove_session, session_mut,
};
use crate::launch::models::{LaunchError, LaunchSelection, LaunchingState};
use crate::launcher_config::helpers::java::refresh_and_update_javas;
use crate::launcher_config::models::{
  FileValidatePolicy, JavaInfo, LauncherConfig, LauncherVisiablity,
};
use crate::resource::helpers::misc::get_source_priority_list;
use crate::storage::load_json_async;
use crate::tasks::commands::schedule_progressive_task_group;
use crate::tasks::monitor::TaskMonitor;
use crate::utils::fs::create_zip_from_dirs;
use crate::utils::logging::get_launcher_log_path;
use crate::utils::shell::{execute_command_line, split_command_line};
use crate::utils::window::create_webview_window;
use std::collections::HashMap;
use std::fs;
use std::io::prelude::*;
use std::io::BufReader;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::{Command, Stdio};
use std::sync::{mpsc, Mutex};
use tauri::path::BaseDirectory;
use tauri::{AppHandle, Manager, State};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

fn launchable_instance(
  instance: Option<Instance>,
  pending_instance: impl FnOnce() -> Option<Instance>,
) -> USTBLResult<Instance> {
  let instance = instance
    .or_else(pending_instance)
    .ok_or(InstanceError::InstanceNotFoundByID)?;
  if has_campus_install(&instance.version_path) {
    return Err(LaunchError::LaunchInstanceInstalling.into());
  }
  Ok(instance)
}

// Step 1: select suitable java runtime environment.
#[tauri::command]
pub async fn select_suitable_jre(
  app: AppHandle,
  instance_id: String,
  instances_state: State<'_, Mutex<HashMap<String, Instance>>>,
  javas_state: State<'_, Mutex<Vec<JavaInfo>>>,
  launching_queue_state: State<'_, Mutex<Vec<LaunchingState>>>,
) -> USTBLResult<LaunchSelection> {
  // A desktop shortcut made by an older version carries the old `<game dir>:<folder>`
  // id, so a miss is retried in that form.
  let instance = {
    let table = instances_state.lock()?;
    find_instance(&table, &instance_id).cloned()
  };
  // Pending clients may not have a jar and are absent from the normal launch map.
  let instance = launchable_instance(instance, || {
    find_instance(&pending_campus_instances(&app), &instance_id).cloned()
  })?;
  let instance_id = instance.id.clone();
  let game_config = get_instance_game_config(&app, &instance);

  let client_path = instance
    .version_path
    .join(format!("{}.json", instance.name));
  let client_info = load_json_async::<McClientInfo>(&client_path).await?;

  if let Some(profile) = &client_info.component_profile {
    profile.validate_platform()?;
  }

  refresh_and_update_javas(&app).await;
  let javas = javas_state.lock()?.clone();

  let selected_java = if let Some(profile) = client_info
    .component_profile
    .as_ref()
    .filter(|p| !p.compatible_java_majors.is_empty())
  {
    // GTNH and the Mechanomania (航空学) pack must run on Java 25, overriding
    // whatever compatible majors the component profile declares.
    let accepted = if instance_requires_java_25(&instance) {
      vec![25]
    } else {
      profile.compatible_java_majors.clone()
    };
    select_component_java(&game_config.game_java, &javas, &accepted)?
  } else {
    select_java_runtime(
      &app,
      &game_config.game_java,
      &javas,
      &instance,
      client_info
        .java_version
        .as_ref()
        .map_or(0i32, |v| v.major_version),
    )
    .await?
  };

  // Return the selected runtime so the launch UI can show which Java was picked.
  let selected_java_for_result = selected_java.clone();
  let launching_id = next_launching_id();
  let mut launching = launching_queue_state.lock()?;
  // Hold both registries through queue insertion, including registered update
  // jobs still preparing their plan before they acquire the disk UpdateGuard.
  let root = instance.version_path.clone();
  UpdateGuard::while_idle(&root, || {
    app
      .state::<Pin<Box<TaskMonitor>>>()
      .while_no_modpack_update(&instance_id, || {
        launching.push(LaunchingState {
          id: launching_id,
          game_config,
          client_info,
          selected_java,
          selected_instance: instance,
          ..LaunchingState::default()
        })
      })
  })??;

  Ok(LaunchSelection {
    launching_id,
    java: selected_java_for_result,
  })
}

// Step 2: extract native libraries, validate game and dependency files, and prepare legacy game assets (if needed).
#[tauri::command]
pub async fn validate_game_files(
  app: AppHandle,
  launching_id: u64,
  launcher_config_state: State<'_, Mutex<LauncherConfig>>,
  launching_queue_state: State<'_, Mutex<Vec<LaunchingState>>>,
) -> USTBLResult<()> {
  // The launch UI cannot continue after a failed step 2, so the session leaves the queue.
  let result = validate_game_files_for(
    &app,
    launching_id,
    &launcher_config_state,
    &launching_queue_state,
  )
  .await;
  if result.is_err() {
    remove_session(&launching_queue_state, launching_id);
  }
  result
}

async fn validate_game_files_for(
  app: &AppHandle,
  launching_id: u64,
  launcher_config_state: &Mutex<LauncherConfig>,
  launching_queue_state: &Mutex<Vec<LaunchingState>>,
) -> USTBLResult<()> {
  let (instance, mut client_info, workaround) = {
    let mut launching_queue = launching_queue_state.lock()?;
    let launching = session_mut(&mut launching_queue, launching_id)?;
    launching.current_step = 2;
    (
      launching.selected_instance.clone(),
      launching.client_info.clone(),
      launching.game_config.advanced.workaround.clone(),
    )
  };

  if instance.mod_loader.status != ModLoaderStatus::Installed {
    return Err(LaunchError::ModLoaderNotInstalled.into());
  }

  replace_native_libraries(app, &mut client_info, &instance)
    .await
    .map_err(|_| InstanceError::ClientJsonParseError)?;

  {
    let mut launching_queue = launching_queue_state.lock()?;
    let launching = session_mut(&mut launching_queue, launching_id)?;

    launching.client_info = client_info.clone();
  }

  // extract native libraries
  let dirs = get_instance_subdir_paths(
    app,
    &instance,
    &[
      &InstanceSubdirType::Root,
      &InstanceSubdirType::Libraries,
      &InstanceSubdirType::NativeLibraries,
      &InstanceSubdirType::Assets,
    ],
  )
  .ok_or(InstanceError::InstanceNotFoundByID)?;
  let [root_dir, libraries_dir, natives_dir, assets_dir] = dirs.as_slice() else {
    return Err(InstanceError::InstanceNotFoundByID.into());
  };

  let priority_list = {
    let launcher_config = launcher_config_state.lock()?;
    get_source_priority_list(&launcher_config)
  };

  // validate game files
  let incomplete_files = match workaround.game_file_validate_policy {
    FileValidatePolicy::Disable => Vec::new(), // skip
    FileValidatePolicy::Normal => [
      get_invalid_library_files(&priority_list, libraries_dir, &client_info, false).await?,
      get_invalid_assets(app, &client_info, &priority_list, assets_dir, false).await?,
    ]
    .concat(),
    FileValidatePolicy::Full => [
      get_invalid_library_files(&priority_list, libraries_dir, &client_info, true).await?,
      get_invalid_assets(app, &client_info, &priority_list, assets_dir, true).await?,
    ]
    .concat(),
  };

  if incomplete_files.is_empty() {
    // Missing native jars must be repaired before extraction, not vice versa.
    extract_native_libraries(
      &client_info,
      libraries_dir,
      natives_dir,
      workaround.use_native_glfw,
      workaround.use_native_openal,
    )
    .await?;
    prepare_legacy_assets(root_dir, assets_dir, &client_info.asset_index.id).await?;
    Ok(())
  } else {
    schedule_progressive_task_group(
      app.clone(),
      format!("patch-files?{}", client_info.id),
      incomplete_files,
      true,
    )
    .await?;
    Err(LaunchError::GameFilesIncomplete.into())
  }
}

// Step 3: validate selected player, if its type is 3rd-party, load server meta for authlib.
// returns Ok(false) if the access_token is expired, Ok(true) if the token is valid.
#[tauri::command]
pub async fn validate_selected_player(
  app: AppHandle,
  launching_id: u64,
  launching_queue_state: State<'_, Mutex<Vec<LaunchingState>>>,
  local_ygg_server_state: State<'_, Mutex<YggdrasilServer>>,
) -> USTBLResult<bool> {
  let mut player = get_selected_player_info(&app)?.clone();

  if player.player_type == PlayerType::ThirdParty
    && player.auth_server_url.as_deref().is_some_and(|url| {
      url.trim_end_matches('/')
        == crate::account::helpers::authlib_injector::constants::USTB_AUTH_SERVER_URL
          .trim_end_matches('/')
    })
  {
    crate::account::helpers::vustb::stored_tokens(&app)?;
  }

  if player.player_type == PlayerType::Microsoft {
    if !microsoft::oauth::validate(&app, &player).await? {
      return Ok(false);
    }
    player = get_selected_player_info(&app)?;
  }

  let metadata = if player.player_type == PlayerType::ThirdParty {
    authlib_injector::jar::check_authlib_jar(&app)
      .await
      .map_err(|_| LaunchError::AuthlibInjectorNotReady)?;
    Some(
      authlib_injector::info::get_auth_server_info_by_url(
        &app,
        player.auth_server_url.clone().unwrap_or_default(),
      )?
      .metadata
      .to_string(),
    )
  } else if player.player_type == PlayerType::Offline
    && authlib_injector::jar::check_authlib_jar(&app).await.is_ok()
  {
    let local_ygg_server = local_ygg_server_state.lock()?;
    player.auth_server_url = Some(local_ygg_server.root_url.clone());
    local_ygg_server.apply_player(player.clone());
    Some(local_ygg_server.metadata.to_string())
  } else {
    None
  };

  {
    let mut launching_queue = launching_queue_state.lock()?;
    let launching = session_mut(&mut launching_queue, launching_id)?;
    launching.current_step = 3;
    launching.selected_player = Some(player.clone());
    launching.auth_server_meta = metadata;
  }

  match player.player_type {
    PlayerType::ThirdParty => authlib_injector::common::validate(&app, &player).await,
    PlayerType::Microsoft => Ok(true),
    PlayerType::Offline => Ok(true),
  }
}

#[tauri::command]
pub async fn launch_game(
  app: AppHandle,
  launching_id: u64,
  launching_queue_state: State<'_, Mutex<Vec<LaunchingState>>>,
  quick_play_singleplayer: Option<String>,
  quick_play_multiplayer: Option<String>,
) -> USTBLResult<()> {
  let launching = {
    let mut launching_queue = launching_queue_state.lock()?;
    let launching = session_mut(&mut launching_queue, launching_id)?;
    launching.current_step = 4;
    launching.clone()
  };
  let game_config = launching.game_config.clone();

  // A session that fails before the process is handed to the monitor leaves the queue.
  let rx = match start_game_process(
    &app,
    &launching_queue_state,
    launching,
    quick_play_singleplayer,
    quick_play_multiplayer,
  )
  .await
  {
    Ok(rx) => rx,
    Err(error) => {
      remove_session(&launching_queue_state, launching_id);
      return Err(error);
    }
  };

  // Wait until the game signals that it has started (see `READY_FLAG`).
  //
  // This must NOT block forever: the signal only fires when the game prints one of
  // the known readiness markers on stdout. Modded loaders are free to change their
  // console format, and if none of the markers ever appear the "waiting for game to
  // launch" modal stays on screen with no way out. Wait with a bounded timeout and
  // treat a timeout as "started anyway" - the process is alive at this point, so
  // dismissing the modal is the correct behaviour.
  const GAME_READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
  let ready_rx = tokio::task::spawn_blocking(move || rx.recv_timeout(GAME_READY_TIMEOUT)).await;
  match ready_rx {
    Ok(Ok(())) => {}
    Ok(Err(mpsc::RecvTimeoutError::Timeout)) => {
      log::warn!(
        "Game did not print a readiness marker within {}s; assuming it started.",
        GAME_READY_TIMEOUT.as_secs()
      );
    }
    Ok(Err(mpsc::RecvTimeoutError::Disconnected)) => {
      log::warn!("Game process ended before reporting readiness.");
    }
    Err(error) => {
      log::warn!("Failed to wait for game readiness: {error}");
    }
  }

  if game_config.launcher_visibility != LauncherVisiablity::Always {
    let _ = app
      .get_webview_window("main")
      .expect("no main window")
      .hide();
  }

  Ok(())
}

// Spawns the game and hands it to the process monitor. Returns the readiness receiver.
async fn start_game_process(
  app: &AppHandle,
  launching_queue_state: &Mutex<Vec<LaunchingState>>,
  launching: LaunchingState,
  quick_play_singleplayer: Option<String>,
  quick_play_multiplayer: Option<String>,
) -> USTBLResult<mpsc::Receiver<()>> {
  let id = launching.id;
  let selected_java = launching.selected_java.clone();
  let game_config = launching.game_config.clone();
  let instance = launching.selected_instance.clone();
  let instance_id = instance.id.clone();
  let work_dir = get_instance_subdir_paths(app, &instance, &[&InstanceSubdirType::Root])
    .ok_or(InstanceError::InstanceNotFoundByID)?
    .first()
    .ok_or(InstanceError::InstanceNotFoundByID)?
    .clone();

  // The game log must exist before the process does: a failure here must not leave a
  // running game that nothing is reading the output of.
  let log_file = create_game_log(app, id)?;

  // generate launch command
  let LaunchCommand {
    class_paths,
    args: cmd_args,
  } = generate_launch_command(
    app,
    &launching,
    quick_play_singleplayer,
    quick_play_multiplayer,
  )
  .await?;

  let wrapper = game_config
    .advanced
    .custom_commands
    .wrapper_launcher
    .clone();

  let mut cmd_base = if let Some(mut c) = split_command_line(&wrapper)? {
    c.arg(&selected_java.exec_path);
    c
  } else {
    Command::new(&selected_java.exec_path)
  };

  let full_cmd = export_full_launch_command(&class_paths, &cmd_args, &selected_java.exec_path);
  println!("[Launch Command] {}", full_cmd);

  let precall_cmd = game_config.advanced.custom_commands.precall_command.clone();
  if !precall_cmd.trim().is_empty() {
    let _ = execute_command_line(&precall_cmd);
  }

  // execute launch command
  #[cfg(target_os = "windows")]
  cmd_base.creation_flags(0x08000000);

  let mut child = cmd_base
    .current_dir(&work_dir)
    .env("CLASSPATH", class_paths.join(get_separator()))
    .args(cmd_args)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()?;

  let pid = child.id();

  // set process priority (if error, keep silent)
  let _ = set_process_priority(pid, &game_config.performance.process_priority);

  // The session may have been cancelled while the process was starting; then nobody is
  // waiting for this game, so stop it instead of leaving it running unmonitored.
  let registered = match launching_queue_state.lock() {
    Ok(mut launching_queue) => {
      register_game_process(&mut launching_queue, id, pid, full_cmd).map_err(USTBLError::from)
    }
    Err(error) => Err(USTBLError::from(error)),
  };
  if let Err(error) = registered {
    let _ = tokio::task::spawn_blocking(move || {
      let _ = child.kill();
      let _ = child.wait();
    })
    .await;
    return Err(error);
  }

  // wait for the game window, create log window if needed
  let (tx, rx) = mpsc::channel();
  crate::account::helpers::vustb_presence::track_game_started(
    id,
    instance.shown_name().to_string(),
  );
  let presence_app = app.clone();
  tauri::async_runtime::spawn(async move {
    let _ = crate::account::helpers::vustb_presence::sync(&presence_app).await;
  });
  monitor_process(
    app.clone(),
    id,
    child,
    log_file,
    instance_id,
    game_config.display_game_log,
    &game_config.game_window.custom_title,
    game_config.launcher_visibility.clone(),
    tx,
    Some(
      game_config
        .advanced
        .custom_commands
        .post_exit_command
        .clone(),
    ),
  )
  .await;

  Ok(rx)
}

#[tauri::command]
pub fn cancel_launch_process(
  launching_id: u64,
  launching_queue_state: State<'_, Mutex<Vec<LaunchingState>>>,
) -> USTBLResult<()> {
  let mut launching_queue = launching_queue_state.lock()?;
  cancel_session(&mut launching_queue, launching_id, kill_process)
}

#[tauri::command]
pub async fn open_game_log_window(app: AppHandle, launching_id: u64) -> USTBLResult<()> {
  create_webview_window(&app, &format!("game_log_{launching_id}"), "game_log", None).await?;

  Ok(())
}

#[tauri::command]
pub fn retrieve_game_log(app: AppHandle, launching_id: u64) -> USTBLResult<Vec<String>> {
  let log_file_dir = game_log_path(&app, launching_id)?;
  Ok(
    BufReader::new(std::fs::OpenOptions::new().read(true).open(log_file_dir)?)
      .lines()
      .map_while(Result::ok)
      .collect(),
  )
}

#[tauri::command]
pub fn retrieve_game_launching_state(
  launching_queue_state: State<'_, Mutex<Vec<LaunchingState>>>,
  launching_id: u64,
) -> USTBLResult<LaunchingState> {
  let launching_queue = launching_queue_state.lock()?;
  if let Some(launching) = launching_queue.iter().find(|l| l.id == launching_id) {
    Ok(launching.clone())
  } else {
    Err(LaunchError::LaunchingStateNotFound.into())
  }
}

#[tauri::command]
pub fn export_game_crash_info(
  app: AppHandle,
  launching_queue_state: State<'_, Mutex<Vec<LaunchingState>>>,
  launching_id: u64,
  save_path: String,
) -> USTBLResult<String> {
  // game log
  let game_log_path = game_log_path(&app, launching_id)?;

  // crash report
  let crash_report_path =
    parse_crash_report_path_from_log(&game_log_path).filter(|path| path.exists());

  // version json and ustbl instance config
  let launching_queue = launching_queue_state.lock()?;
  let launching = launching_queue
    .iter()
    .find(|l| l.id == launching_id)
    .ok_or(LaunchError::LaunchingStateNotFound)?;
  let version_info_path = launching
    .selected_instance
    .version_path
    .join(format!("{}.json", launching.selected_instance.name));
  let version_config_path = launching.selected_instance.get_json_cfg_path();

  // full launch script
  let launch_script_path = app.path().resolve::<PathBuf>(
    if cfg!(target_os = "windows") {
      "launch.bat".into()
    } else {
      "launch.sh".into()
    },
    BaseDirectory::Temp,
  )?;
  fs::write(&launch_script_path, &launching.full_command)?;

  // launcher log
  let launcher_log_path = get_launcher_log_path(app.clone());

  let zip_file_path = PathBuf::from(save_path);

  let mut paths_to_zip = vec![
    game_log_path,
    version_info_path,
    version_config_path,
    launch_script_path,
    launcher_log_path,
  ];

  paths_to_zip.extend(crash_report_path);
  create_zip_from_dirs(paths_to_zip, zip_file_path.clone())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn unfinished_installs_are_rejected_before_client_or_java_access() {
    let root = std::env::temp_dir().join(format!("ustbl-launch-install-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(root.join(".ustbl-update")).unwrap();
    let record = root.join(".ustbl-update/campus-install.json");
    for phase in ["pending", "waitingClient", "installing", "failed"] {
      fs::write(
        &record,
        serde_json::to_vec(&serde_json::json!({
          "instanceId": "test:pending", "instanceName": "pending",
          "label": "Test", "phase": phase, "error": null
        }))
        .unwrap(),
      )
      .unwrap();
      let instance = Instance {
        version_path: root.to_path_buf(),
        ..Instance::default()
      };
      assert_eq!(
        launchable_instance(Some(instance.clone()), || panic!("unexpected scan"))
          .unwrap_err()
          .0,
        "LAUNCH_INSTANCE_INSTALLING"
      );
      // A summary-only instance still reports installing, not missing instance/JSON.
      assert_eq!(
        launchable_instance(None, || Some(instance)).unwrap_err().0,
        "LAUNCH_INSTANCE_INSTALLING"
      );
    }
    fs::remove_file(record).unwrap();
    assert!(launchable_instance(
      Some(Instance {
        version_path: root.to_path_buf(),
        ..Instance::default()
      }),
      || panic!("unexpected scan")
    )
    .is_ok());
    assert_eq!(
      launchable_instance(None, || None).unwrap_err().0,
      "INSTANCE_NOT_FOUND_BY_ID"
    );
    fs::remove_dir_all(root).unwrap();
  }

  fn running_game() -> std::process::Child {
    #[cfg(target_os = "windows")]
    let mut command = {
      let mut command = Command::new("cmd");
      command.args(["/C", "ping -n 30 127.0.0.1 >NUL"]);
      command.creation_flags(0x08000000);
      command
    };
    #[cfg(not(target_os = "windows"))]
    let mut command = {
      let mut command = Command::new("sleep");
      command.arg("30");
      command
    };
    command.stdout(Stdio::null()).stderr(Stdio::null());
    command.spawn().unwrap()
  }

  fn launching_queue_app(queue: Vec<LaunchingState>) -> tauri::App<tauri::test::MockRuntime> {
    let app = tauri::test::mock_app();
    app.manage(Mutex::new(queue));
    app
  }

  // Step 1 failed (no id yet), the user pressed cancel while game A was running. The old
  // `cancel_launch_process` took the queue tail and killed game A.
  #[test]
  fn cancelling_a_launch_that_never_got_an_id_leaves_the_running_game_alone() {
    let mut game_a = running_game();
    let app = launching_queue_app(vec![LaunchingState {
      id: 1,
      current_step: 4,
      pid: game_a.id(),
      ..LaunchingState::default()
    }]);

    // An id that was never issued must be a no-op.
    cancel_launch_process(2, app.state()).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(
      game_a.try_wait().unwrap().is_none(),
      "cancelling an unknown launch killed another running game"
    );
    assert_eq!(
      app.state::<Mutex<Vec<LaunchingState>>>().lock().unwrap()[0].current_step,
      4
    );

    // Cancelling game A by its own id still works and only marks that session.
    cancel_launch_process(1, app.state()).unwrap();
    assert!(!game_a.wait().unwrap().success());
    assert_eq!(
      app.state::<Mutex<Vec<LaunchingState>>>().lock().unwrap()[0].current_step,
      0
    );
  }

  #[test]
  fn two_sessions_of_the_same_instance_are_cancelled_independently() {
    let mut game_a = running_game();
    let mut game_b = running_game();
    let app = launching_queue_app(vec![
      LaunchingState {
        id: 10,
        current_step: 4,
        pid: game_a.id(),
        ..LaunchingState::default()
      },
      LaunchingState {
        id: 11,
        current_step: 4,
        pid: game_b.id(),
        ..LaunchingState::default()
      },
    ]);

    // The older session is cancelled although it is not the queue tail.
    cancel_launch_process(10, app.state()).unwrap();
    assert!(!game_a.wait().unwrap().success());
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(game_b.try_wait().unwrap().is_none());

    cancel_launch_process(11, app.state()).unwrap();
    assert!(!game_b.wait().unwrap().success());
  }

  #[test]
  fn a_cancelled_step_one_session_leaves_the_queue_and_later_steps_fail_cleanly() {
    let app = launching_queue_app(vec![LaunchingState {
      id: 5,
      ..LaunchingState::default()
    }]);
    cancel_launch_process(5, app.state()).unwrap();
    assert!(app
      .state::<Mutex<Vec<LaunchingState>>>()
      .lock()
      .unwrap()
      .is_empty());
    assert_eq!(
      retrieve_game_launching_state(app.state(), 5).unwrap_err().0,
      "LAUNCHING_STATE_NOT_FOUND"
    );
  }
}
