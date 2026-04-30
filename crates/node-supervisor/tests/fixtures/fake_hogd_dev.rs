use std::{
    env,
    io::{self, Read, Write},
    process,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};

const MODE_ENV: &str = "GROUNDHOG_FAKE_HOGD_DEV_MODE";
const READY: &str = r#"{"status":"ready","network_id":"groundhog-local","endpoints":{"scheduler":"127.0.0.1:12345","read":"127.0.0.1:12346","control":"127.0.0.1:12347"}}"#;
static TERMINATED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_sigterm(_: i32) {
    TERMINATED.store(true, Ordering::SeqCst);
}

fn main() {
    match env::var(MODE_ENV).as_deref() {
        Ok("ready") => run_ready(),
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

fn run_ready() {
    install_sigterm_handler();
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
    loop {
        thread::sleep(Duration::from_secs(60));
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
    unsafe {
        libc::signal(
            libc::SIGTERM,
            handle_sigterm as *const () as libc::sighandler_t,
        );
    }
}

fn ignore_sigterm() {
    // SAFETY: sets SIGTERM disposition to ignore for this fake child process.
    unsafe {
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
    }
}
