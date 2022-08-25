use async_compat::CompatExt;
use smol::channel::Receiver;
use std::{io, time::Duration};
use wezterm_ssh::{Config, ExecResult, Session, SessionEvent};

// Command to run (on Windows)
const CMD: &str = "cmd.exe /C echo %OS%";

// Time to wait inbetween requests to get stdout/stderr from cmd
const READER_PAUSE_MILLIS: u64 = 100;

// SSH configuration settings
const HOST: &str = "";
const PORT: Option<u16> = None;
const USER: Option<&str> = None;
const BACKEND: &str = "ssh2";

// Set this without checking it in so we provide some default answers to auth prompts
const ANSWERS: &[&str] = &[];

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Establishing ssh connection to {HOST}, port = {PORT:?}");
    let mut config = Config::new();
    config.add_default_config_files();

    // Grab the config for the specific host
    let mut config = config.for_host(HOST);

    // Override config with any settings provided by client opts
    if let Some(port) = PORT {
        config.insert("port".to_string(), port.to_string());
    }
    if let Some(user) = USER {
        config.insert("user".to_string(), user.to_string());
    }

    // Set verbosity optin for ssh lib
    config.insert("wezterm_ssh_verbose".to_string(), "true".to_string());

    // Set the backend to use going forward
    config.insert("wezterm_ssh_backend".to_string(), BACKEND.to_string());

    // Port should always exist, otherwise Session will panic from unwrap()
    let _ = config.get("port").expect("Missing port");

    // Establish a connection
    println!("Session::connect({:?})", config);
    let (session, events) = Session::connect(config)?;

    // Authentication
    println!("Authenticating...");
    authenticate(events).await?;

    // Perform command and get results
    println!("Executing {CMD}");
    let output = execute_cmd(&session, CMD).await?;

    // Print output
    println!("Success = {}", output.success);
    println!("Stdout = '{}'", String::from_utf8_lossy(&output.stdout));
    println!("Stderr = '{}'", String::from_utf8_lossy(&output.stderr));

    Ok(())
}

#[derive(Debug)]
pub struct Output {
    pub success: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

async fn execute_cmd(session: &Session, cmd: &str) -> Result<Output, Box<dyn std::error::Error>> {
    let ExecResult {
        mut child,
        mut stderr,
        mut stdout,
        ..
    } = session.exec(cmd, None).compat().await?;

    macro_rules! spawn_reader {
        ($reader:ident) => {{
            $reader.set_non_blocking(true)?;
            tokio::spawn(async move {
                use std::io::Read;
                let mut bytes = Vec::new();
                let mut buf = [0u8; 1024];
                loop {
                    match $reader.read(&mut buf) {
                        Ok(n) if n > 0 => bytes.extend(&buf[..n]),
                        Ok(_) => break Ok(bytes),
                        Err(x) if x.kind() == io::ErrorKind::WouldBlock => {
                            tokio::time::sleep(Duration::from_millis(READER_PAUSE_MILLIS)).await;
                        }
                        Err(x) => break Err(x),
                    }
                }
            })
        }};
    }

    // Spawn async readers for stdout and stderr from process
    let stdout_handle = spawn_reader!(stdout);
    let stderr_handle = spawn_reader!(stderr);

    // Wait for our handles to conclude
    let stdout = stdout_handle.await??;
    let stderr = stderr_handle.await??;

    // Wait for process to conclude
    let status = child.async_wait().compat().await?;

    Ok(Output {
        success: status.success(),
        stdout,
        stderr,
    })
}

async fn authenticate(events: Receiver<SessionEvent>) -> Result<(), Box<dyn std::error::Error>> {
    // Perform the authentication by listening for events and continuing to handle them
    // until authenticated
    while let Ok(event) = events.recv().await {
        match event {
            // Will trust anything
            SessionEvent::HostVerify(verify) => {
                verify
                    .answer(true)
                    .compat()
                    .await
                    .map_err(|x| io::Error::new(io::ErrorKind::Other, x))?;
            }

            // Will provide answer from our static definition
            SessionEvent::Authenticate(auth) => {
                auth.answer(ANSWERS.iter().copied().map(ToString::to_string).collect())
                    .compat()
                    .await
                    .map_err(|x| io::Error::new(io::ErrorKind::Other, x))?;
            }

            // Prints out banner if we get it
            SessionEvent::Banner(banner) => {
                if let Some(banner) = banner {
                    println!("Banner: {banner}");
                }
            }

            // Fails if we get an error
            SessionEvent::Error(err) => {
                return Err(Box::new(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    err,
                )));
            }

            // Done with authentication
            SessionEvent::Authenticated => break,
        }
    }

    Ok(())
}
