#![cfg(all(target_os = "linux", not(debug_assertions)))]

use camellia_remote_protocol::config::{Config, APP_NAME};
use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::Command;

const CHILD_ENV: &str = "CAMELLIA_CRASH_SIGNAL_TEST_CHILD";
const ROOT_ENV: &str = "CAMELLIA_CRASH_SIGNAL_TEST_ROOT";

#[inline(never)]
fn nvidia_release_sigsegv_probe() -> ! {
    unsafe {
        libc::raise(libc::SIGSEGV);
    }
    std::process::abort();
}

fn contains_regular_file(path: &Path) -> bool {
    let Ok(entries) = fs::read_dir(path) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() || (path.is_dir() && contains_regular_file(&path)) {
            return true;
        }
    }
    false
}

#[test]
fn release_sigsegv_uses_default_os_disposition() {
    if std::env::var_os(CHILD_ENV).is_some() {
        let root = std::env::var_os(ROOT_ENV).expect("child test root must be provided");
        let root = std::path::PathBuf::from(root);
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        *APP_NAME.write().expect("app name lock must be available") =
            format!("CamelliaCrashSignalTest{}", std::process::id());
        let _ = Config::get_option("enable-hwcodec");

        unsafe {
            libc::signal(libc::SIGSEGV, libc::SIG_DFL);
        }
        nvidia_release_sigsegv_probe();
    }

    let root = std::env::temp_dir().join(format!(
        "camellia-protocol-crash-signal-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("isolated child root must be created");

    let status = Command::new(std::env::current_exe().expect("test executable must be available"))
        .arg("--exact")
        .arg("release_sigsegv_uses_default_os_disposition")
        .arg("--nocapture")
        .env(CHILD_ENV, "1")
        .env(ROOT_ENV, &root)
        .status()
        .expect("child test process must start");

    let callback_ran = root.join("callback-ran").exists();
    let config_written = contains_regular_file(&root.join("config"));
    let signal = status.signal();
    let exit_code = status.code();
    let _ = fs::remove_dir_all(&root);

    assert!(
        signal == Some(libc::SIGSEGV) && !callback_ran && !config_written,
        "signal={signal:?}, exit_code={exit_code:?}, callback_ran={callback_ran}, config_written={config_written}"
    );
}
