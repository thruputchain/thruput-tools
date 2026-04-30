use std::{
    env, fs,
    io::{self, Read, Write},
    process::{self, Command},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};

const MODE_ENV: &str = "GROUNDHOG_FAKE_HOGD_DEV_MODE";
/// Optional path the fake writes its PID to before printing readiness.
/// Tests use this to verify cleanup (poll `kill(pid, 0)` until ESRCH).
const PID_FILE_ENV: &str = "GROUNDHOG_FAKE_HOGD_DEV_PID_FILE";
const READY: &str = r#"{"status":"ready","network_id":"groundhog-local","endpoints":{"scheduler":"127.0.0.1:12345","read":"127.0.0.1:12346","control":"127.0.0.1:12347"}}"#;
static TERMINATED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_sigterm(_: i32) {
    TERMINATED.store(true, Ordering::SeqCst);
}

fn main() {
    match env::var(MODE_ENV).as_deref() {
        Ok("ready") => run_ready(),
        Ok("ready-then-exit") => run_ready_then_exit(),
        Ok("spawn-helper-ignore-term") => run_with_helper(),
        Ok("helper-ignore-term") => run_ignore_term_loop(),
        Ok("ignore-term") => run_ignore_term(),
        Ok("exit-before-ready") => {
            read_genesis();
            process::exit(9);
        }
        Ok("timeout") => {
            read_genesis();
            eprintln!(r#"{{"level":"info","message":"waiting"}}"#);
            loop {
                thread::sleep(Duration::from_secs(60));
            }
        }
        Ok("oversized-readiness") => {
            read_genesis();
            io::stdout()
                .write_all(&vec![b'x'; 70_000])
                .expect("bug: fake child stdout should be writable");
            io::stdout()
                .flush()
                .expect("bug: fake child stdout should flush");
            loop {
                thread::sleep(Duration::from_secs(60));
            }
        }
        Ok("exit-7") => process::exit(7),
        Ok("hang") => loop {
            thread::sleep(Duration::from_secs(60));
        },
        _ => {}
    }
}

fn run_ready_then_exit() {
    write_pid_file_if_requested();
    read_genesis();
    println!("{READY}");
    eprintln!(r#"{{"level":"info","target":"hogd_dev","message":"ready-then-exit"}}"#);
    // Flush stdout/stderr so the supervisor sees readiness before the
    // process disappears.
    let _ = io::stdout().flush();
    let _ = io::stderr().flush();
    process::exit(0);
}

fn run_ready() {
    install_sigterm_handler();
    write_pid_file_if_requested();
    read_genesis();
    println!("{READY}");
    eprintln!(r#"{{"level":"info","target":"hogd_dev","message":"ready"}}"#);

    while !TERMINATED.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(10));
    }

    eprintln!(r#"{{"level":"info","target":"hogd_dev","message":"stopping"}}"#);
}

fn run_ignore_term() {
    ignore_sigterm();
    read_genesis();
    println!("{READY}");
    run_ignore_term_loop();
}

#[allow(clippy::zombie_processes)]
fn run_with_helper() {
    install_sigterm_handler();
    let current_exe = env::current_exe().expect("bug: fake child binary path should resolve");
    // The parent intentionally exits without waiting so the supervisor test can
    // prove process-group SIGKILL reaches descendants after leader exit.
    let _helper = Command::new(current_exe)
        .env(MODE_ENV, "helper-ignore-term")
        .spawn()
        .expect("bug: helper process should spawn");

    read_genesis();
    println!("{READY}");

    while !TERMINATED.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(10));
    }
}

fn run_ignore_term_loop() {
    loop {
        thread::sleep(Duration::from_secs(60));
    }
}

fn write_pid_file_if_requested() {
    if let Ok(path) = env::var(PID_FILE_ENV) {
        fs::write(&path, process::id().to_string())
            .expect("bug: fake child should be able to write its pid file");
    }
}

fn read_genesis() {
    let mut genesis = Vec::new();
    io::stdin()
        .read_to_end(&mut genesis)
        .expect("bug: fake child stdin should be readable");
}

fn install_sigterm_handler() {
    // SAFETY: installs a process-local handler that only stores to an atomic.
    #[allow(function_casts_as_integer)]
    let handler = handle_sigterm as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGTERM, handler);
    }
}

fn ignore_sigterm() {
    // SAFETY: sets SIGTERM disposition to ignore for this fake child process.
    unsafe {
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
    }
}
