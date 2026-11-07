use std::path::PathBuf;

#[cfg(unix)]
use std::path::Path;

use hostler::paths::{Paths, SocketName};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[test]
fn under_uses_fixed_file_names() {
    let home = PathBuf::from("/state/hostler");
    let paths = Paths::under(home.clone(), true);

    assert_eq!(paths.home, home);
    assert!(paths.custom_home);
    assert_eq!(paths.db, home.join("hostler.db"));
    assert_eq!(paths.service_lock, home.join("service.lock"));
    assert_eq!(paths.log_dir, home.join("logs"));
}

#[cfg(unix)]
#[test]
fn long_home_falls_back_to_private_socket_dir() {
    assert_eq!(
        Paths::under(PathBuf::from("/state/hostler"), false).socket,
        SocketName::File(PathBuf::from("/state/hostler/hostler.sock"))
    );

    let home = PathBuf::from(format!("/{}", "a".repeat(119)));
    assert_eq!(home.as_os_str().len(), 120);
    assert!(home.join("hostler.sock").as_os_str().len() > 100);

    let paths = Paths::under(home.clone(), false);
    let SocketName::File(socket) = &paths.socket else {
        panic!("unix must use a socket file, got {:?}", paths.socket);
    };
    assert!(socket.as_os_str().len() <= 100);
    assert_eq!(socket.file_name().unwrap(), "hostler.sock");

    let dir = socket.parent().unwrap();
    assert_eq!(dir.parent(), Some(Path::new("/tmp")));
    assert!(
        socket.to_str().unwrap().starts_with("/tmp/"),
        "fallback must live under the literal /tmp, not $TMPDIR: {}",
        socket.display()
    );
    let name = dir.file_name().unwrap().to_str().unwrap();
    let hash = name
        .strip_prefix(&format!("hostler-{}-", current_uid()))
        .unwrap();
    assert_eq!(hash.len(), 16);
    assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));

    assert_eq!(Paths::under(home, false).socket, paths.socket);
}

#[cfg(unix)]
#[test]
fn ensure_dirs_creates_private_dirs() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("h".repeat(120));
    let paths = Paths::under(home, true);
    assert!(paths.home.join("hostler.sock").as_os_str().len() > 100);
    let socket_dir = private_socket_dir(&paths);
    let _cleanup = RemoveDir(socket_dir.clone());

    paths.ensure_dirs().unwrap();

    for dir in [&paths.home, &paths.log_dir, &socket_dir] {
        assert!(dir.is_dir(), "{} was not created", dir.display());
        assert_eq!(mode(dir), 0o700, "{} is not private", dir.display());
    }
    paths.ensure_dirs().unwrap();
}

#[cfg(unix)]
#[test]
fn ensure_dirs_refuses_open_permissions() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("h".repeat(120));
    let paths = Paths::under(home, true);
    let socket_dir = private_socket_dir(&paths);
    let _cleanup = RemoveDir(socket_dir.clone());

    std::fs::create_dir_all(&socket_dir).unwrap();
    std::fs::set_permissions(&socket_dir, std::fs::Permissions::from_mode(0o777)).unwrap();

    let err = paths.ensure_dirs().unwrap_err();
    let message = format!("{err:#}");
    assert!(
        message.contains(socket_dir.to_str().unwrap()),
        "error does not name {}: {message}",
        socket_dir.display()
    );
}

#[cfg(windows)]
#[test]
fn windows_uses_named_pipe() {
    let paths = Paths::under(PathBuf::from(r"C:\state\hostler"), false);
    let SocketName::Namespaced(name) = &paths.socket else {
        panic!("windows must use a named pipe, got {:?}", paths.socket);
    };
    assert!(name.starts_with("hostler-"));
    let hash = name.rsplit('-').next().unwrap();
    assert_eq!(hash.len(), 16);
    assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
}

#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: geteuid takes no arguments, cannot fail and touches no memory.
    unsafe { libc::geteuid() }
}

#[cfg(unix)]
fn private_socket_dir(paths: &Paths) -> PathBuf {
    match &paths.socket {
        SocketName::File(socket) => socket.parent().unwrap().to_path_buf(),
        SocketName::Namespaced(_) => panic!("expected a unix socket file"),
    }
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[cfg(unix)]
struct RemoveDir(PathBuf);

#[cfg(unix)]
impl Drop for RemoveDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
