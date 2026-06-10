use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::process::Child;

use crate::aws::discovery;
use crate::aws::profiles::{self, AwsConfig};
use crate::aws::rds::{self, RdsInstance};
use crate::aws::sso::{self, TokenStatus};
use crate::config::{self, Config};
use crate::tunnel::{Group, Tunnel, TunnelKind};

const TOAST_TTL: Duration = Duration::from_secs(6);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Profiles,
    Tunnels,
}

pub struct Toast {
    pub message: String,
    pub error: bool,
    pub at: Instant,
}

/// RDS instance picker overlay.
pub struct RdsPicker {
    pub items: Vec<RdsInstance>,
    pub selected: usize,
    pub profile: String,
}

pub struct App {
    pub config: Config,
    pub config_path: PathBuf,
    pub aws: AwsConfig,
    pub session_status: BTreeMap<String, TokenStatus>,
    pub active_profile: usize,
    pub profile_sel: usize,
    pub tunnels: Vec<Tunnel>,
    pub tunnel_sel: usize,
    pub focus: Focus,
    pub picker: Option<RdsPicker>,
    pub toast: Option<Toast>,
    pub login_children: BTreeMap<String, Child>,
    pub should_quit: bool,
}

impl App {
    pub fn new() -> Result<App> {
        let (config, config_path) = Config::load()?;
        let aws = profiles::load()?;

        let active_profile = aws
            .profiles
            .iter()
            .position(|p| p.name == "default")
            .unwrap_or(0);

        let mut app = App {
            config,
            config_path,
            aws,
            session_status: BTreeMap::new(),
            active_profile,
            profile_sel: active_profile,
            tunnels: Vec::new(),
            tunnel_sel: 0,
            focus: Focus::Tunnels,
            picker: None,
            toast: None,
            login_children: BTreeMap::new(),
            should_quit: false,
        };

        app.rescan_sso();
        app.build_seed_tunnels();
        Ok(app)
    }

    /// Build the always-present tunnels: configured services + temporal entries.
    fn build_seed_tunnels(&mut self) {
        let mut used_ports: HashSet<u16> = self.config.services.ports.values().copied().collect();
        let mut next_auto = self.config.services.auto_port_base;

        for name in &self.config.services.fallback {
            let port = self
                .config
                .services
                .ports
                .get(name)
                .copied()
                .unwrap_or_else(|| {
                    while used_ports.contains(&next_auto) {
                        next_auto += 1;
                    }
                    used_ports.insert(next_auto);
                    next_auto
                });
            self.tunnels.push(Tunnel::new(
                name.clone(),
                Group::Services,
                TunnelKind::SsmService {
                    service_name: name.clone(),
                },
                Some(self.config.services.profile.clone()),
                self.config.region.clone(),
                Some(self.config.services.bastion.clone()),
                port,
            ));
        }

        for t in &self.config.temporal {
            self.tunnels.push(Tunnel::new(
                t.name.clone(),
                Group::Temporal,
                TunnelKind::Ssh {
                    pem: config::expand_tilde(&t.pem),
                    elb_host: t.elb_host.clone(),
                    remote_port: t.remote_port,
                    ec2_host: t.ec2_host.clone(),
                    ec2_user: t.ec2_user.clone(),
                },
                None,
                self.config.region.clone(),
                None,
                t.local_port,
            ));
        }
    }

    pub fn rescan_sso(&mut self) {
        let mut map = BTreeMap::new();
        for name in self.aws.sessions.keys() {
            map.insert(name.clone(), sso::scan_session(name));
        }
        self.session_status = map;
    }

    /// Tunnel indices in display (grouped) order.
    pub fn ordered(&self) -> Vec<usize> {
        let mut v = Vec::new();
        for g in Group::ALL {
            for (i, t) in self.tunnels.iter().enumerate() {
                if t.group == g {
                    v.push(i);
                }
            }
        }
        v
    }

    pub fn selected_tunnel(&self) -> Option<usize> {
        self.ordered().get(self.tunnel_sel).copied()
    }

    pub fn active_profile_name(&self) -> &str {
        self.aws
            .profiles
            .get(self.active_profile)
            .map(|p| p.name.as_str())
            .unwrap_or("default")
    }

    pub fn active_session_name(&self) -> Option<String> {
        self.aws
            .profiles
            .get(self.active_profile)
            .and_then(|p| p.sso_session.clone())
    }

    pub fn status_for_profile(&self, idx: usize) -> TokenStatus {
        self.aws
            .profiles
            .get(idx)
            .and_then(|p| p.sso_session.as_ref())
            .and_then(|s| self.session_status.get(s).copied())
            .unwrap_or(TokenStatus::NoToken)
    }

    fn set_toast(&mut self, message: impl Into<String>, error: bool) {
        self.toast = Some(Toast {
            message: message.into(),
            error,
            at: Instant::now(),
        });
    }

    // ---- actions -----------------------------------------------------------

    pub async fn start_selected(&mut self) {
        if let Some(i) = self.selected_tunnel() {
            self.tunnels[i].start().await;
        }
    }

    pub fn stop_selected(&mut self) {
        if let Some(i) = self.selected_tunnel() {
            self.tunnels[i].stop();
        }
    }

    pub async fn start_group(&mut self, group: Group) {
        let indices: Vec<usize> = self
            .tunnels
            .iter()
            .enumerate()
            .filter(|(_, t)| t.group == group && !t.is_active())
            .map(|(i, _)| i)
            .collect();
        for i in indices {
            self.tunnels[i].start().await;
        }
    }

    pub fn stop_group(&mut self, group: Group) {
        for t in self.tunnels.iter_mut().filter(|t| t.group == group) {
            if t.is_active() {
                t.stop();
            }
        }
    }

    pub fn selected_group(&self) -> Option<Group> {
        self.selected_tunnel().map(|i| self.tunnels[i].group)
    }

    pub async fn open_rds_picker(&mut self) {
        let profile = self.active_profile_name().to_string();
        let region = self.config.region.clone();
        self.set_toast(format!("discovering RDS in {profile}…"), false);
        match rds::list(&profile, &region).await {
            Ok(items) if !items.is_empty() => {
                self.picker = Some(RdsPicker {
                    items,
                    selected: 0,
                    profile,
                });
                self.toast = None;
            }
            Ok(_) => self.set_toast(format!("no RDS instances visible to {profile}"), true),
            Err(e) => self.set_toast(format!("RDS discovery failed: {e}"), true),
        }
    }

    pub async fn confirm_rds_pick(&mut self) {
        let Some(picker) = self.picker.take() else {
            return;
        };
        let Some(inst) = picker.items.get(picker.selected).cloned() else {
            return;
        };

        let session = self.active_session_name();
        let bastion = session
            .as_ref()
            .and_then(|s| self.config.rds.bastions.get(s).cloned());
        let Some(bastion) = bastion else {
            self.set_toast(
                format!(
                    "no RDS bastion configured for session '{}' — add it under [rds.bastions]",
                    session.as_deref().unwrap_or("?")
                ),
                true,
            );
            return;
        };

        let local_port = self.next_local_port(self.config.rds.local_port_base);
        let name = format!("{} ({})", inst.identifier, picker.profile);
        let mut tunnel = Tunnel::new(
            name,
            Group::Databases,
            TunnelKind::SsmRds {
                endpoint: inst.endpoint.clone(),
                remote_port: inst.port,
            },
            Some(picker.profile.clone()),
            self.config.region.clone(),
            Some(bastion),
            local_port,
        );
        tunnel.start().await;
        self.tunnels.push(tunnel);
        // Select the newly added database tunnel.
        if let Some(pos) = self
            .ordered()
            .iter()
            .position(|&i| i == self.tunnels.len() - 1)
        {
            self.tunnel_sel = pos;
            self.focus = Focus::Tunnels;
        }
    }

    fn next_local_port(&self, base: u16) -> u16 {
        let used: HashSet<u16> = self.tunnels.iter().map(|t| t.local_port).collect();
        let mut p = base;
        while used.contains(&p) {
            p = p.saturating_add(1);
        }
        p
    }

    /// Start `aws sso login` for the focused profile's session (Profiles panel)
    /// or the active profile's session otherwise.
    pub fn trigger_login(&mut self) {
        let idx = if self.focus == Focus::Profiles {
            self.profile_sel
        } else {
            self.active_profile
        };
        let Some(session) = self
            .aws
            .profiles
            .get(idx)
            .and_then(|p| p.sso_session.clone())
        else {
            self.set_toast("selected profile has no sso-session", true);
            return;
        };
        if self.login_children.contains_key(&session) {
            self.set_toast(format!("login for '{session}' already in progress"), false);
            return;
        }
        match sso::login(&session) {
            Ok(child) => {
                self.login_children.insert(session.clone(), child);
                self.set_toast(
                    format!("logging in to '{session}' — complete it in your browser"),
                    false,
                );
            }
            Err(e) => self.set_toast(format!("failed to start aws sso login: {e}"), true),
        }
    }

    pub async fn refresh(&mut self) {
        self.rescan_sso();

        // Live Cloud Map discovery: append any services not already shown.
        let profile = self.config.services.profile.clone();
        let region = self.config.region.clone();
        match discovery::list_services(&profile, &region).await {
            Ok(names) => {
                let mut added = 0;
                for name in names {
                    let exists = self
                        .tunnels
                        .iter()
                        .any(|t| t.group == Group::Services && t.name == name);
                    if exists {
                        continue;
                    }
                    let port = self
                        .config
                        .services
                        .ports
                        .get(&name)
                        .copied()
                        .unwrap_or_else(|| {
                            self.next_local_port(self.config.services.auto_port_base)
                        });
                    self.tunnels.push(Tunnel::new(
                        name.clone(),
                        Group::Services,
                        TunnelKind::SsmService { service_name: name },
                        Some(profile.clone()),
                        region.clone(),
                        Some(self.config.services.bastion.clone()),
                        port,
                    ));
                    added += 1;
                }
                if added > 0 {
                    self.set_toast(
                        format!("refreshed — discovered {added} new service(s)"),
                        false,
                    );
                } else {
                    self.set_toast("refreshed", false);
                }
            }
            Err(e) => self.set_toast(
                format!("refreshed SSO; Cloud Map discovery failed: {e}"),
                true,
            ),
        }
    }

    // ---- selection movement ------------------------------------------------

    pub fn move_up(&mut self) {
        match self.focus {
            Focus::Profiles => {
                if self.profile_sel > 0 {
                    self.profile_sel -= 1;
                }
            }
            Focus::Tunnels => {
                if self.tunnel_sel > 0 {
                    self.tunnel_sel -= 1;
                }
            }
        }
    }

    pub fn move_down(&mut self) {
        match self.focus {
            Focus::Profiles => {
                let max = self.aws.profiles.len().saturating_sub(1);
                if self.profile_sel < max {
                    self.profile_sel += 1;
                }
            }
            Focus::Tunnels => {
                let max = self.ordered().len().saturating_sub(1);
                if self.tunnel_sel < max {
                    self.tunnel_sel += 1;
                }
            }
        }
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Profiles => Focus::Tunnels,
            Focus::Tunnels => Focus::Profiles,
        };
    }

    pub fn set_active_to_selected(&mut self) {
        self.active_profile = self.profile_sel;
        self.set_toast(
            format!("active profile → {}", self.active_profile_name()),
            false,
        );
    }

    // ---- lifecycle ---------------------------------------------------------

    pub async fn on_tick(&mut self) {
        for i in 0..self.tunnels.len() {
            self.tunnels[i].tick().await;
        }

        // Reap completed `aws sso login` children, then rescan.
        let mut finished = Vec::new();
        for (name, child) in self.login_children.iter_mut() {
            if let Ok(Some(_)) = child.try_wait() {
                finished.push(name.clone());
            }
        }
        if !finished.is_empty() {
            for name in &finished {
                self.login_children.remove(name);
            }
            self.rescan_sso();
            self.set_toast(format!("login complete: {}", finished.join(", ")), false);
        }

        if let Some(t) = &self.toast {
            if t.at.elapsed() > TOAST_TTL {
                self.toast = None;
            }
        }
    }

    pub async fn shutdown(&mut self) {
        for t in self.tunnels.iter_mut() {
            if t.is_active() {
                t.stop();
            }
        }
        // Give SIGTERM a moment to tear down session-manager-plugin / ssh.
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
