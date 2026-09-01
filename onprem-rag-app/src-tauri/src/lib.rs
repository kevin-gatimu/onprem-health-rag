mod commands;
mod state;

use state::Bridge;

/// Default server URL when the app hasn't been pointed elsewhere. Overridable at
/// runtime via the `set_server_url` command (the frontend reads its own
/// `VITE_DEFAULT_SERVER_URL` and pushes it down on startup).
const DEFAULT_SERVER_URL: &str = "http://localhost:8000";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut builder = tauri::Builder::default();

    // Desktop-only: single-instance must be the FIRST registered plugin so a second
    // launch is intercepted before anything else initializes; window-state restores
    // size/position from the previous run.
    #[cfg(desktop)]
    {
        builder = builder
            .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
                use tauri::Manager;
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.unminimize();
                    let _ = window.set_focus();
                }
            }))
            .plugin(tauri_plugin_window_state::Builder::default().build());
    }

    builder
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .manage(Bridge::new(DEFAULT_SERVER_URL.to_string()))
        .setup(|app| {
            use tauri::Manager;
            use tauri_plugin_store::StoreExt;
            let bridge = app.state::<Bridge>();
            match app.store("bridge-store.json") {
                Err(e) => {
                    // First run or corrupt store — not fatal; the app starts with defaults.
                    eprintln!("bridge-store: could not open on startup: {e}");
                }
                Ok(store) => {
                    if let Some(serde_json::Value::String(url)) = store.get("base_url") {
                        bridge.set_base_url(url);
                    }
                    if let Some(serde_json::Value::String(token)) = store.get("token") {
                        bridge.set_token(Some(token));
                    }
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_server_url,
            commands::set_server_url,
            commands::health,
            commands::login,
            commands::me,
            commands::logout,
            commands::is_authenticated,
            commands::get_stats,
            commands::get_hardware,
            commands::list_models,
            commands::select_model,
            commands::set_role_model,
            commands::register_eps,
            commands::model_roles,
            commands::pull_model,
            commands::delete_model,
            commands::get_setup_status,
            commands::unload_model,
            commands::generate,
            commands::list_sources,
            commands::test_source,
            commands::save_source,
            commands::update_source,
            commands::test_saved_source,
            commands::delete_source,
            commands::start_ingest,
            commands::get_schema,
            commands::analyze_schema,
            commands::get_ingest_history,
            commands::delete_ingest_table,
            commands::list_records,
            commands::get_table_info,
            commands::delete_ingest_connection,
            commands::clear_all_records,
            commands::search,
            commands::chat,
            commands::agent,
            commands::cancel_run,
            commands::start_log_stream,
            commands::list_conversations,
            commands::list_agent_conversations,
            commands::create_conversation,
            commands::rename_conversation,
            commands::delete_conversation,
            commands::get_messages,
            commands::list_users,
            commands::create_user,
            commands::update_user,
            commands::delete_user,
            commands::set_user_password,
            commands::update_me,
            commands::change_my_password,
            commands::get_audit,
            commands::force_logout_user,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
