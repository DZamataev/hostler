//! L58: in debug builds a service started by a test process carries `HOSTLER_TEST_OWNER_PID`
//! and stops through its normal shutdown path (exit 0, socket removed) once that process is gone,
//! so a killed or timed-out run leaves no daemons behind. `Home::hostler()` sets the hook to this
//! test process; a service the client starts in-process gets it from the library (FR1-1); these
//! tests override it with a helper process they control.

mod common;

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::Home;

/// A process that lives until this test kills it.
fn helper() -> Child {
    #[cfg(unix)]
    let mut command = {
        let mut c = Command::new("sleep");
        c.arg("30");
        c
    };
    #[cfg(windows)]
    let mut command = {
        let mut c = Command::new("ping");
        c.args(["-n", "30", "127.0.0.1"]);
        c
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("a helper process")
}

/// `hostler service run` built by `Home::hostler()`, watching `pid` as its test owner.
fn service(home: &Home, pid: u32) -> Child {
    home.hostler()
        .env("HOSTLER_TEST_OWNER_PID", pid.to_string())
        .args(["service", "run"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

/// Exit code 0 is `service status`'s "running"; 3 is "not running".
fn status_code(home: &Home) -> Option<i32> {
    home.hostler()
        .args(["service", "status"])
        .output()
        .unwrap()
        .status
        .code()
}

fn service_up_within(home: &Home, limit: Duration) {
    let deadline = Instant::now() + limit;
    while status_code(home) != Some(0) {
        assert!(Instant::now() < deadline, "the service never came up");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Bounded wait for the service process itself: `true` when it exited successfully.
fn exits_within(service: &mut Child, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = service.try_wait().unwrap() {
            return status.success();
        }
        assert!(
            Instant::now() < deadline,
            "the service still runs {limit:?} after its test owner went away"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Bounded wait until `service status` says not running and the socket is gone.
fn stopped_within(home: &Home, limit: Duration) {
    let deadline = Instant::now() + limit;
    loop {
        #[cfg(unix)]
        let socket_gone = {
            let hostler::paths::SocketName::File(socket) = home.paths().socket else {
                unreachable!("Unix homes use a file socket");
            };
            !socket.exists()
        };
        #[cfg(windows)]
        let socket_gone = true;
        if status_code(home) == Some(3) && socket_gone {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the service still runs {limit:?} after its test owner went away"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn a_service_stops_when_its_test_owner_dies() {
    let home = Home::new();
    let mut owner = helper();
    let mut service = service(&home, owner.id());
    service_up_within(&home, Duration::from_secs(15));

    owner.kill().unwrap();
    owner.wait().unwrap();
    assert!(
        exits_within(&mut service, Duration::from_secs(15)),
        "the service must exit 0 through its normal shutdown path"
    );
    stopped_within(&home, Duration::from_secs(15));
}

#[test]
fn a_service_stops_at_once_when_its_test_owner_never_existed() {
    let home = Home::new();
    let mut owner = helper();
    let dead = owner.id();
    owner.kill().unwrap();
    owner.wait().unwrap();

    // The owner's identity cannot be read at start, so no liveness tick is waited for.
    let mut service = service(&home, dead);
    assert!(exits_within(&mut service, Duration::from_secs(15)));
    stopped_within(&home, Duration::from_secs(15));
}

/// FR1-1: a service `Client::connect_or_start_with` starts in-process (no `hostler` command of
/// ours can set the hook on it) still watches the test owner named through the library.
#[cfg(debug_assertions)]
#[test]
fn an_in_process_start_carries_the_test_owner() {
    use hostler::client::{Client, Replace};
    use hostler::contract::wire::Request;
    use hostler::service::NoManager;

    let home = Home::new();
    let mut owner = helper();
    hostler::client::set_test_owner_pid(owner.id());
    let paths = home.paths();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut client =
            Client::connect_or_start_with(&paths, &NoManager, common::BIN.as_ref(), Replace::Never)
                .await
                .expect("the client starts the service itself");
        client
            .call(Request::ServiceStatus)
            .await
            .expect("a service answers");
    });

    owner.kill().unwrap();
    owner.wait().unwrap();
    stopped_within(&home, Duration::from_secs(15));
    // Leave no owner for whatever this test binary runs next.
    hostler::client::set_test_owner_pid(0);
}
