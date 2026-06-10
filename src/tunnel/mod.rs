use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

use crate::aws::discovery;

const LOG_CAP: usize = 200;
const HEALTH_TIMEOUT: Duration = Duration::from_millis(200);

/// Which panel group a tunnel belongs to. Order is the display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    Services,
    Databases,
    Temporal,
}

impl Group {
    pub fn title(&self) -> &'static str {
        match self {
            Group::Services => "Services",
            Group::Databases => "Databases",
            Group::Temporal => "Temporal",
        }
    }
    pub const ALL: [Group; 3] = [Group::Services, Group::Databases, Group::Temporal];
}

/// How a tunnel is established.
#[derive(Debug, Clone)]
pub enum TunnelKind {
    /// SSM port-forward to a Cloud Map service; endpoint resolved live at start.
    SsmService { service_name: String },
    /// SSM port-forward to a fixed remote host (RDS endpoint).
    SsmRds { endpoint: String, remote_port: u16 },
    /// `ssh -L` through a bastion (Temporal UIs).
    Ssh {
        pem: PathBuf,
        elb_host: String,
        remote_port: u16,
        ec2_host: String,
        ec2_user: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Stopped,
    Starting,
    Running,
    Failed(String),
    /// Local port already bound by something we didn't spawn (e.g. a manual script).
    External,
}

impl Status {
    pub fn label(&self) -> &str {
        match self {
            Status::Stopped => "stopped",
            Status::Starting => "starting",
            Status::Running => "running",
            Status::Failed(_) => "failed",
            Status::External => "external",
        }
    }
}

pub struct Tunnel {
    pub name: String,
    pub group: Group,
    pub kind: TunnelKind,
    /// AWS profile used for SSM tunnels; `None` for pure ssh.
    pub profile: Option<String>,
    pub region: String,
    /// Bastion instance id for SSM tunnels; `None` for ssh.
    pub bastion: Option<String>,
    pub local_port: u16,
    pub status: Status,
    pub log: Arc<Mutex<Vec<String>>>,
    child: Option<Child>,
    pgid: Option<i32>,
}

impl Tunnel {
    pub fn new(
        name: String,
        group: Group,
        kind: TunnelKind,
        profile: Option<String>,
        region: String,
        bastion: Option<String>,
        local_port: u16,
    ) -> Self {
        Tunnel {
            name,
            group,
            kind,
            profile,
            region,
            bastion,
            local_port,
            status: Status::Stopped,
            log: Arc::new(Mutex::new(Vec::new())),
            child: None,
            pgid: None,
        }
    }

    pub fn is_active(&self) -> bool {
        self.child.is_some() || matches!(self.status, Status::Starting | Status::Running)
    }

    fn push_log(&self, line: impl Into<String>) {
        let mut g = self.log.lock().unwrap();
        g.push(line.into());
        if g.len() > LOG_CAP {
            let excess = g.len() - LOG_CAP;
            g.drain(0..excess);
        }
    }

    /// Bring the tunnel up. Resolves Cloud Map endpoints for services, validates
    /// pem permissions for ssh, then spawns the child in its own process group.
    pub async fn start(&mut self) {
        if self.child.is_some() {
            return;
        }
        // Don't fight a port someone else already holds.
        if port_is_open(self.local_port).await {
            self.status = Status::External;
            self.push_log(format!(
                "local port {} already in use — not starting (external)",
                self.local_port
            ));
            return;
        }

        self.status = Status::Starting;
        self.log.lock().unwrap().clear();

        let (program, args) = match self.build_command().await {
            Ok(c) => c,
            Err(e) => {
                self.push_log(format!("start failed: {e}"));
                self.status = Status::Failed(e);
                return;
            }
        };

        self.push_log(format!("$ {} {}", program, args.join(" ")));

        let mut cmd = Command::new(&program);
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true);

        match cmd.spawn() {
            Ok(mut child) => {
                self.pgid = child.id().map(|p| p as i32);
                if let Some(stdout) = child.stdout.take() {
                    spawn_reader(stdout, self.log.clone());
                }
                if let Some(stderr) = child.stderr.take() {
                    spawn_reader(stderr, self.log.clone());
                }
                self.child = Some(child);
                // Stays Starting; the health tick promotes it to Running.
            }
            Err(e) => {
                let msg = format!("failed to spawn {program}: {e}");
                self.push_log(&msg);
                self.status = Status::Failed(msg);
            }
        }
    }

    /// Build the (program, args) for this tunnel kind.
    async fn build_command(&self) -> Result<(String, Vec<String>), String> {
        match &self.kind {
            TunnelKind::SsmService { service_name } => {
                let profile = self.profile.as_deref().unwrap_or("default");
                let (ip, remote_port) = discovery::resolve(profile, &self.region, service_name)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(self.ssm_args(&ip, remote_port))
            }
            TunnelKind::SsmRds {
                endpoint,
                remote_port,
            } => Ok(self.ssm_args(endpoint, *remote_port)),
            TunnelKind::Ssh {
                pem,
                elb_host,
                remote_port,
                ec2_host,
                ec2_user,
            } => {
                check_pem(pem)?;
                let forward = format!("{}:{}:{}", self.local_port, elb_host, remote_port);
                let dest = format!("{ec2_user}@{ec2_host}");
                let args = vec![
                    "-i".to_string(),
                    pem.to_string_lossy().to_string(),
                    "-L".to_string(),
                    forward,
                    dest,
                    "-N".to_string(),
                    "-o".to_string(),
                    "StrictHostKeyChecking=accept-new".to_string(),
                    "-o".to_string(),
                    "ExitOnForwardFailure=yes".to_string(),
                    "-o".to_string(),
                    "ConnectTimeout=10".to_string(),
                    "-o".to_string(),
                    "ServerAliveInterval=30".to_string(),
                    "-o".to_string(),
                    "BatchMode=yes".to_string(),
                ];
                Ok(("ssh".to_string(), args))
            }
        }
    }

    fn ssm_args(&self, host: &str, remote_port: u16) -> (String, Vec<String>) {
        let bastion = self.bastion.clone().unwrap_or_default();
        let profile = self
            .profile
            .clone()
            .unwrap_or_else(|| "default".to_string());
        let parameters = format!(
            "{{\"host\":[\"{host}\"],\"portNumber\":[\"{remote_port}\"],\"localPortNumber\":[\"{}\"]}}",
            self.local_port
        );
        let args = vec![
            "ssm".to_string(),
            "start-session".to_string(),
            "--target".to_string(),
            bastion,
            "--region".to_string(),
            self.region.clone(),
            "--profile".to_string(),
            profile,
            "--document-name".to_string(),
            "AWS-StartPortForwardingSessionToRemoteHost".to_string(),
            "--parameters".to_string(),
            parameters,
        ];
        ("aws".to_string(), args)
    }

    /// Signal the whole process group (so `aws` and the session-manager-plugin it
    /// forks both die), then drop the child.
    pub fn stop(&mut self) {
        if let Some(pgid) = self.pgid.take() {
            unsafe {
                libc::kill(-pgid, libc::SIGTERM);
            }
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
        }
        self.status = Status::Stopped;
        self.push_log("stopped");
    }

    /// Per-tick maintenance: detect child exit and probe local-port health.
    pub async fn tick(&mut self) {
        // Detect an exited child first.
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(exit)) => {
                    self.child = None;
                    self.pgid = None;
                    if matches!(self.status, Status::Starting | Status::Running) {
                        let detail = self.last_log_line().unwrap_or_else(|| exit.to_string());
                        self.status = Status::Failed(detail);
                    } else if self.status != Status::Stopped {
                        self.status = Status::Stopped;
                    }
                    return;
                }
                Ok(None) => {}
                Err(_) => {}
            }
        }

        let open = port_is_open(self.local_port).await;
        match (&self.status, self.child.is_some(), open) {
            (Status::Starting, true, true) => self.status = Status::Running,
            (Status::Running, true, false) => self.status = Status::Starting,
            // Track tunnels brought up outside moleman (manual scripts).
            (Status::Stopped, false, true) => self.status = Status::External,
            (Status::External, false, false) => self.status = Status::Stopped,
            _ => {}
        }
    }

    fn last_log_line(&self) -> Option<String> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|l| !l.trim().is_empty())
            .cloned()
    }

    /// The command that would be run, for the detail pane (no live resolution).
    pub fn command_preview(&self) -> String {
        match &self.kind {
            TunnelKind::SsmService { service_name } => format!(
                "aws ssm start-session --target {} --profile {} (resolve {} via Cloud Map) -> localhost:{}",
                self.bastion.as_deref().unwrap_or("?"),
                self.profile.as_deref().unwrap_or("?"),
                service_name,
                self.local_port
            ),
            TunnelKind::SsmRds { endpoint, remote_port } => format!(
                "aws ssm start-session --target {} --profile {} {}:{} -> localhost:{}",
                self.bastion.as_deref().unwrap_or("?"),
                self.profile.as_deref().unwrap_or("?"),
                endpoint,
                remote_port,
                self.local_port
            ),
            TunnelKind::Ssh { elb_host, remote_port, ec2_host, ec2_user, .. } => format!(
                "ssh {}@{} -L {}:{}:{}",
                ec2_user, ec2_host, self.local_port, elb_host, remote_port
            ),
        }
    }
}

/// Read a child stream line by line into the shared, capped log buffer.
fn spawn_reader<R>(reader: R, log: Arc<Mutex<Vec<String>>>)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let mut g = log.lock().unwrap();
            g.push(line);
            if g.len() > LOG_CAP {
                let excess = g.len() - LOG_CAP;
                g.drain(0..excess);
            }
        }
    });
}

/// True if something accepts a TCP connection on `127.0.0.1:port`.
pub async fn port_is_open(port: u16) -> bool {
    let addr = format!("127.0.0.1:{port}");
    matches!(
        tokio::time::timeout(HEALTH_TIMEOUT, tokio::net::TcpStream::connect(&addr)).await,
        Ok(Ok(_))
    )
}

/// ssh refuses keys readable by group/other. Surface that clearly instead of a
/// confusing ssh error, and confirm the file exists.
fn check_pem(pem: &PathBuf) -> Result<(), String> {
    let meta = std::fs::metadata(pem).map_err(|_| format!("pem not found: {}", pem.display()))?;
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(format!(
            "pem {} has permissions {:o} — ssh requires 0600 or stricter (chmod 600)",
            pem.display(),
            mode
        ));
    }
    Ok(())
}
