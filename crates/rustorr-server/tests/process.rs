//! Runs the real `rustorr` binary: startup, serving, signals, exit codes.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use serde_json::Value;

const STARTUP: Duration = Duration::from_secs(60);
const EXIT: Duration = Duration::from_secs(20);

struct Server {
    child: Child,
    lines: mpsc::Receiver<String>,
    events: Vec<Value>,
    address: Option<SocketAddr>,
}

struct Exit {
    status: ExitStatus,
    elapsed: Duration,
}

impl Server {
    /// Starts the binary offline, on a free port, logging JSON unless told
    /// otherwise, and does not wait for it.
    fn spawn(args: &[&str], env: &[(&str, &str)]) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rustorr"));
        command.args(["--disable-dht", "--disable-trackers"]);
        // A free port unless the test picks the address itself.
        if !args.contains(&"--listen") {
            command.args(["--listen", "127.0.0.1:0"]);
        }
        let mut child = command
            .args(args)
            .envs(env.iter().copied())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the rustorr binary starts");
        let stderr = child.stderr.take().unwrap();
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            lines,
            events: Vec::new(),
            address: None,
        }
    }

    /// Starts the binary and waits until it reports that it is listening.
    fn start(data_dir: &Path, extra: &[&str]) -> Self {
        let mut args = vec![
            "--data-dir",
            data_dir.to_str().unwrap(),
            "--log-format",
            "json",
        ];
        args.extend_from_slice(extra);
        let mut server = Self::spawn(&args, &[]);
        server.wait_until_listening();
        server
    }

    fn read_event(&mut self, deadline: Instant) -> Option<()> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let line = self.lines.recv_timeout(remaining).ok()?;
        let event = serde_json::from_str(&line).unwrap_or(Value::String(line));
        self.events.push(event);
        Some(())
    }

    fn wait_until_listening(&mut self) {
        let deadline = Instant::now() + STARTUP;
        loop {
            if let Some(address) = self.events.iter().find_map(listening_address) {
                self.address = Some(address);
                return;
            }
            self.read_event(deadline)
                .unwrap_or_else(|| panic!("never listened. log so far: {:#?}", self.events));
        }
    }

    fn address(&self) -> SocketAddr {
        self.address.expect("the server is listening")
    }

    fn signal(&self, name: &str) {
        let status = Command::new("kill")
            .arg(format!("-{name}"))
            .arg(self.child.id().to_string())
            .status()
            .expect("kill runs");
        assert!(status.success(), "kill -{name} failed");
    }

    /// Waits for the process to exit, then collects the rest of its log.
    fn wait_for_exit(&mut self) -> Exit {
        let started = Instant::now();
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(
                started.elapsed() < EXIT,
                "still running after {EXIT:?}; log: {:#?}",
                self.events
            );
            thread::sleep(Duration::from_millis(25));
        };
        let elapsed = started.elapsed();
        while self
            .read_event(Instant::now() + Duration::from_secs(2))
            .is_some()
        {}
        Exit { status, elapsed }
    }

    fn messages(&self) -> Vec<String> {
        self.events
            .iter()
            .map(|event| match event["fields"]["message"].as_str() {
                Some(message) => message.to_owned(),
                None => event.to_string(),
            })
            .collect()
    }

    fn position(&self, message: &str) -> usize {
        self.messages()
            .iter()
            .position(|m| m == message)
            .unwrap_or_else(|| panic!("no `{message}` in {:#?}", self.messages()))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn listening_address(event: &Value) -> Option<SocketAddr> {
    if event["fields"]["message"] == "listening" {
        return event["fields"]["address"].as_str()?.parse().ok();
    }
    // Plain-text log lines arrive as strings: `... listening address=1.2.3.4:5 ...`.
    let line = event.as_str()?;
    if !line.contains("listening") {
        return None;
    }
    let tail = line.split("address=").nth(1)?;
    tail.split_whitespace().next()?.parse().ok()
}

/// A bare HTTP/1.1 exchange, so the test needs no HTTP client.
fn http_get(address: SocketAddr, path: &str) -> (String, String) {
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let (head, body) = response
        .split_once("\r\n\r\n")
        .expect("a complete response");
    (head.lines().next().unwrap().to_owned(), body.to_owned())
}

fn assert_in_order(server: &Server, messages: &[&str]) {
    let positions: Vec<usize> = messages.iter().map(|m| server.position(m)).collect();
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "expected {messages:?} in this order, got positions {positions:?} in {:#?}",
        server.messages()
    );
}

fn signal_name(server: &Server) -> String {
    server
        .events
        .iter()
        .find(|event| event["fields"]["message"] == "shutdown signal received")
        .and_then(|event| event["fields"]["signal"].as_str())
        .unwrap_or("<none>")
        .to_owned()
}

fn shuts_down_cleanly_on(signal: &str, expected_name: &str) {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    // The data directory comes from the environment, everything else from flags.
    let mut server = Server::spawn(
        &["--log-format", "json"],
        &[("RUSTORR_DATA_DIR", data.to_str().unwrap())],
    );
    server.wait_until_listening();

    let (status, body) = http_get(server.address(), "/echo");
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(body, "MatriX.145");
    let (status, body) = http_get(server.address(), "/no/such/route");
    assert_eq!(status, "HTTP/1.1 404 Not Found");
    assert_eq!(body, "404 page not found");

    server.signal(signal);
    let exit = server.wait_for_exit();

    assert!(
        exit.status.success(),
        "{:?}; log: {:#?}",
        exit.status,
        server.messages()
    );
    assert_eq!(signal_name(&server), expected_name);
    assert_in_order(
        &server,
        &[
            "listening",
            "shutdown signal received",
            "http server stopped",
            "engine stopped",
            "state closed",
            "shutdown complete",
        ],
    );
    assert!(
        data.join("rustorr.db").is_file(),
        "the database must have been created"
    );
}

#[test]
fn serves_requests_and_shuts_down_cleanly_on_sigterm() {
    shuts_down_cleanly_on("TERM", "SIGTERM");
}

#[test]
fn shuts_down_cleanly_on_sigint_too() {
    shuts_down_cleanly_on("INT", "SIGINT");
}

#[test]
fn text_logs_also_work_and_report_the_address() {
    let dir = tempfile::tempdir().unwrap();
    let mut server = Server::spawn(&["--data-dir", dir.path().to_str().unwrap()], &[]);
    server.wait_until_listening();

    assert_eq!(http_get(server.address(), "/echo").0, "HTTP/1.1 200 OK");
    server.signal("TERM");
    assert!(server.wait_for_exit().status.success());
}

#[test]
fn restarts_on_the_same_data_directory_without_touching_what_is_stored() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("rustorr.db");

    let mut first = Server::start(dir.path(), &[]);
    first.signal("TERM");
    assert!(first.wait_for_exit().status.success());
    {
        let state = rustorr_state::State::open(&database).unwrap();
        assert_eq!(state.schema_version().unwrap(), 2);
        state.set_settings("{\"CacheSize\": 12345}").unwrap();
    }

    let mut second = Server::start(dir.path(), &[]);
    let started = second
        .events
        .iter()
        .find(|event| event["fields"]["message"] == "listening")
        .expect("the second start listens");
    assert_eq!(started["fields"]["schema_version"], 2);
    assert_eq!(http_get(second.address(), "/echo").0, "HTTP/1.1 200 OK");
    second.signal("TERM");
    assert!(second.wait_for_exit().status.success());

    let state = rustorr_state::State::open(&database).unwrap();
    assert_eq!(
        state.settings().unwrap().as_deref(),
        Some("{\"CacheSize\": 12345}")
    );
}

#[test]
fn http_auth_requires_a_valid_nonempty_account_database_and_never_logs_secrets() {
    for contents in [
        None,
        Some(b"".as_slice()),
        Some(b"not-json".as_slice()),
        Some(b"{}".as_slice()),
        Some(br#"{"user":"do-not-log-this-secret""#.as_slice()),
    ] {
        let dir = tempfile::tempdir().unwrap();
        if let Some(contents) = contents {
            std::fs::write(dir.path().join("accs.db"), contents).unwrap();
        }
        let mut server = Server::spawn(
            &[
                "--data-dir",
                dir.path().to_str().unwrap(),
                "--http-auth",
                "--log-format",
                "json",
            ],
            &[],
        );
        let exit = server.wait_for_exit();
        assert_eq!(exit.status.code(), Some(1), "{:?}", server.messages());
        let messages = server.messages().join("\n");
        assert!(messages.contains("authentication database"), "{messages}");
        assert!(!messages.contains("do-not-log-this-secret"), "{messages}");
    }

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("accs.db"),
        br#"{"contract":"do-not-log-this-secret"}"#,
    )
    .unwrap();
    let mut server = Server::start(dir.path(), &["--http-auth"]);
    assert!(
        server
            .messages()
            .iter()
            .all(|message| !message.contains("do-not-log-this-secret"))
    );
    server.signal("TERM");
    assert!(server.wait_for_exit().status.success());
}

#[test]
fn a_connection_that_never_finishes_does_not_keep_the_server_alive() {
    let dir = tempfile::tempdir().unwrap();
    let mut server = Server::start(dir.path(), &["--shutdown-grace", "1"]);
    // Headers that never end: a client that connected and then went silent.
    let mut stuck = TcpStream::connect(server.address()).unwrap();
    stuck
        .write_all(b"GET /echo HTTP/1.1\r\nHost: t\r\n")
        .unwrap();
    thread::sleep(Duration::from_millis(300));

    server.signal("TERM");
    let exit = server.wait_for_exit();

    assert!(
        exit.status.success(),
        "{:?}; log: {:#?}",
        exit.status,
        server.messages()
    );
    // It waited out the one-second grace period, and no longer than that plus
    // the engine's own stop.
    assert!(
        exit.elapsed >= Duration::from_secs(1),
        "gave up too early: {:?}",
        exit.elapsed
    );
    assert!(
        exit.elapsed < Duration::from_secs(10),
        "took {:?}",
        exit.elapsed
    );
    let messages = server.messages();
    assert!(
        messages
            .iter()
            .any(|m| m == "open connections did not finish in time and were closed"),
        "{messages:#?}"
    );
    assert!(
        !messages.iter().any(|m| m == "http server stopped"),
        "{messages:#?}"
    );
    assert_in_order(
        &server,
        &[
            "shutdown signal received",
            "engine stopped",
            "state closed",
            "shutdown complete",
        ],
    );
    drop(stuck);
}

#[test]
fn refuses_to_start_when_the_port_is_taken_before_touching_the_data_directory() {
    let taken = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = taken.local_addr().unwrap().to_string();
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");

    let mut server = Server::spawn(
        &[
            "--data-dir",
            data.to_str().unwrap(),
            "--log-format",
            "json",
            "--listen",
            &address,
        ],
        &[],
    );
    let exit = server.wait_for_exit();

    assert_eq!(exit.status.code(), Some(1));
    let failure = server.messages().pop().unwrap();
    assert!(
        failure.contains("cannot listen on") && failure.contains(&address),
        "{failure}"
    );
    assert!(
        !data.exists(),
        "a failed start must not leave a data directory behind"
    );
}

#[test]
fn refuses_to_start_when_the_data_directory_cannot_be_created() {
    let dir = tempfile::tempdir().unwrap();
    let occupied = dir.path().join("occupied");
    std::fs::write(&occupied, b"").unwrap();

    let mut server = Server::spawn(
        &[
            "--data-dir",
            occupied.join("data").to_str().unwrap(),
            "--log-format",
            "json",
        ],
        &[],
    );
    let exit = server.wait_for_exit();

    assert_eq!(exit.status.code(), Some(1));
    let failure = server.messages().pop().unwrap();
    assert!(
        failure.contains("cannot create the data directory"),
        "{failure}"
    );
}

#[test]
fn refuses_a_database_file_that_is_not_a_database_and_leaves_it_alone() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("rustorr.db");
    std::fs::write(&database, b"these are my notes, not SQLite").unwrap();

    let mut server = Server::spawn(
        &[
            "--data-dir",
            dir.path().to_str().unwrap(),
            "--log-format",
            "json",
        ],
        &[],
    );
    let exit = server.wait_for_exit();

    assert_eq!(exit.status.code(), Some(1));
    let failure = server.messages().pop().unwrap();
    // The message carries the whole chain, down to SQLite's own words.
    assert!(failure.contains("cannot open the database"), "{failure}");
    assert!(failure.contains("file is not a database"), "{failure}");
    assert_eq!(
        std::fs::read(&database).unwrap(),
        b"these are my notes, not SQLite"
    );
}

#[test]
fn reports_its_version_and_rejects_bad_arguments_with_a_usage_error() {
    let version = Command::new(env!("CARGO_BIN_EXE_rustorr"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(version.status.success());
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        concat!("rustorr ", env!("CARGO_PKG_VERSION"))
    );

    let bad = Command::new(env!("CARGO_BIN_EXE_rustorr"))
        .args(["--cache", "tape"])
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("tape"));
}
