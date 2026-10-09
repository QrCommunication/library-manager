use tauri::Manager;

mod commands;

#[cfg(target_os = "linux")]
fn single_instance_available() -> bool {
    use std::os::unix::fs::FileTypeExt;

    match std::env::var_os("DBUS_SESSION_BUS_ADDRESS") {
        Some(address) => address.to_str().is_some_and(valid_unix_bus_address),
        None => std::env::var_os("XDG_RUNTIME_DIR").is_some_and(|directory| {
            let path = std::path::PathBuf::from(directory);
            path.is_absolute()
                && path
                    .join("bus")
                    .metadata()
                    .is_ok_and(|metadata| metadata.file_type().is_socket())
        }),
    }
}

#[cfg(not(target_os = "linux"))]
fn single_instance_available() -> bool {
    true
}

#[cfg(target_os = "linux")]
fn valid_unix_bus_address(address: &str) -> bool {
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::fs::FileTypeExt;
    use std::os::unix::net::{SocketAddr, UnixStream};

    let Some(fields) = address.strip_prefix("unix:") else {
        return false;
    };
    let mut endpoint = None;
    let mut has_guid = false;
    for field in fields.split(',') {
        let Some((key, value)) = field.split_once('=') else {
            return false;
        };
        let Some(_decoded) = decode_bus_address_value(value) else {
            return false;
        };
        match key {
            "path" if endpoint.is_none() => {
                let path = std::path::PathBuf::from(value);
                endpoint = Some(
                    path.is_absolute()
                        && path
                            .metadata()
                            .is_ok_and(|metadata| metadata.file_type().is_socket()),
                );
            }
            "abstract" if endpoint.is_none() => {
                endpoint = Some(
                    SocketAddr::from_abstract_name(value.as_bytes())
                        .is_ok_and(|socket| UnixStream::connect_addr(&socket).is_ok()),
                );
            }
            "guid"
                if !has_guid
                    && value.len() == 32
                    && value.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
            {
                has_guid = true
            }
            _ => return false,
        }
    }
    endpoint == Some(true)
}

#[cfg(all(test, target_os = "linux"))]
mod single_instance_tests {
    use super::{decode_bus_address_value, valid_unix_bus_address};
    use std::os::unix::net::UnixListener;

    #[test]
    fn malformed_values_are_rejected() {
        for value in ["", "%", "%0", "%ZZ", "%00", "bad value", "semi;colon"] {
            assert!(decode_bus_address_value(value).is_none());
        }
        assert_eq!(
            decode_bus_address_value("/tmp/session%20bus"),
            Some(b"/tmp/session bus".to_vec())
        );
    }

    #[test]
    fn validates_socket_and_raw_guid_before_plugin_initialization() {
        struct SocketFile(std::path::PathBuf);
        impl Drop for SocketFile {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = SocketFile(
            std::env::temp_dir().join(format!("lm-bus-{}-{unique}", std::process::id())),
        );
        let _listener = UnixListener::bind(&path.0).unwrap();
        let address = format!("unix:path={}", path.0.display());
        assert!(valid_unix_bus_address(&address));
        assert!(valid_unix_bus_address(&format!(
            "{address},guid=0123456789abcdef0123456789abcdef"
        )));
        assert!(!valid_unix_bus_address(&format!("{address},guid=short")));
        assert!(!valid_unix_bus_address(&format!(
            "{address},guid=%30123456789abcdef0123456789abcdef"
        )));
        assert!(!valid_unix_bus_address(&format!(
            "{address},path={}",
            path.0.display()
        )));
        assert!(!valid_unix_bus_address(&format!("{address},unknown=value")));
        assert!(!valid_unix_bus_address(
            "unix:path=/missing-library-manager-session-bus"
        ));
        assert!(!valid_unix_bus_address("tcp:host=localhost,port=9000"));
    }
}

#[cfg(target_os = "linux")]
fn decode_bus_address_value(value: &str) -> Option<Vec<u8>> {
    let mut decoded = Vec::with_capacity(value.len());
    let mut bytes = value.bytes();
    while let Some(byte) = bytes.next() {
        let decoded_byte = if byte == b'%' {
            let high = char::from(bytes.next()?).to_digit(16)?;
            let low = char::from(bytes.next()?).to_digit(16)?;
            (high * 16 + low) as u8
        } else if byte.is_ascii_alphanumeric() || b"-_/.\\".contains(&byte) {
            byte
        } else {
            return None;
        };
        if decoded_byte == 0 {
            return None;
        }
        decoded.push(decoded_byte);
    }
    (!decoded.is_empty()).then_some(decoded)
}

pub fn run() {
    if std::env::args_os()
        .skip(1)
        .any(|argument| argument == "--version" || argument == "-V")
    {
        println!("Library Manager {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    let builder = tauri::Builder::default();
    let builder = if single_instance_available() {
        builder.plugin(tauri_plugin_single_instance::init(
            |app, _arguments, _directory| {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.unminimize();
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            },
        ))
    } else {
        builder
    };

    builder
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            app.manage(commands::AppState::new(app.handle().clone()));
            app.state::<commands::AppState>()
                .start_background(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_bootstrap,
            commands::library_list,
            commands::library_facets,
            commands::book_get,
            commands::book_files,
            commands::import_books,
            commands::book_update,
            commands::book_enrich,
            commands::book_optimize,
            commands::book_convert,
            commands::conversion_capabilities,
            commands::optimization_profiles,
            commands::devices_scan,
            commands::device_index,
            commands::device_inventory,
            commands::device_import,
            commands::device_connect_wireless,
            commands::device_disconnect,
            commands::device_transfer,
            commands::reader_open,
            commands::reader_section,
            commands::reader_save_progress,
            commands::providers_list,
            commands::provider_models,
            commands::provider_set_secret,
            commands::provider_clear_secret,
            commands::conversations_list,
            commands::conversation_messages,
            commands::chat_send,
            commands::settings_get,
            commands::settings_save,
            commands::jobs_list,
            commands::job_cancel,
            commands::operations_list,
            commands::operation_undo,
        ])
        .run(tauri::generate_context!())
        .expect("Library Manager could not start its native desktop window");
}
