//! Managed public TCP/UDP listeners. The mutex on AppState serializes desired
//! state changes; no public socket admits traffic until its durable commit.

use aperio_config::expose::*;
use aperio_config::expose_policy::ExposePolicy;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use tokio::sync::watch;

use crate::state::AppState;
use crate::store::exposes::{ExposeDocument, ExposeStore};

#[path = "expose_manager/relay.rs"]
mod relay;
#[path = "expose_manager/resolve.rs"]
pub(crate) mod resolve;

#[derive(Clone, Debug)]
pub(crate) struct ManagedRule {
  pub resource: ExposeResource,
  // Never serialized in a management view: old keys are secrets.
  pub legacy: Option<aperio_config::ExposeEntry>,
}

#[derive(Clone)]
pub(crate) struct Control {
  pub rule: Arc<ManagedRule>,
  pub accepting: bool,
  pub terminate: bool,
  pub deadline: Option<Instant>,
}

#[derive(Clone, Serialize, serde::Deserialize)]
pub(crate) struct SessionView {
  pub id: String,
  pub expose_id: String,
  pub org_id: String,
  pub revision: u64,
  pub protocol: ExposeProtocol,
  pub peer: SocketAddr,
  pub client_id: String,
  pub target: String,
  pub started_at: u64,
  pub up_bytes: u64,
  pub down_bytes: u64,
  pub up_packets: u64,
  pub down_packets: u64,
  pub idle_seconds: u64,
  pub ended_at: Option<u64>,
  pub reason: Option<String>,
}

pub(crate) struct Session {
  pub view: SessionView,
  pub touched: Instant,
  pub stop: watch::Sender<bool>,
}

#[derive(Default)]
pub(crate) struct Budget {
  second: Option<Instant>,
  up: u64,
  down: u64,
  opened: u32,
}

impl Budget {
  fn refresh(&mut self) {
    if self
      .second
      .is_none_or(|t| t.elapsed() >= Duration::from_secs(1))
    {
      self.second = Some(Instant::now());
      self.up = 0;
      self.down = 0;
      self.opened = 0;
    }
  }
  pub fn bytes(&mut self, down: bool, bytes: usize, limit: u64) -> bool {
    self.refresh();
    let used = if down { &mut self.down } else { &mut self.up };
    if used.saturating_add(bytes as u64) > limit {
      return false;
    }
    *used += bytes as u64;
    true
  }
  pub fn open(&mut self, limit: u32) -> bool {
    self.refresh();
    if self.opened >= limit {
      return false;
    }
    self.opened += 1;
    true
  }
}

pub(crate) struct Runtime {
  audit_state: Weak<AppState>,
  pub control: watch::Sender<Control>,
  pub status: Mutex<(ExposeListenerState, Option<String>)>,
  pub sessions: Mutex<HashMap<String, Session>>,
  pub history: Mutex<VecDeque<SessionView>>,
  pub budget: Mutex<Budget>,
  pub opened: AtomicU64,
  pub up_bytes: AtomicU64,
  pub down_bytes: AtomicU64,
  pub up_packets: AtomicU64,
  pub down_packets: AtomicU64,
  pub drops: Mutex<BTreeMap<&'static str, u64>>,
}

impl Runtime {
  fn new(rule: ManagedRule, status: ExposeListenerState, state: &Arc<AppState>) -> Arc<Self> {
    let accepting = rule.resource.spec.enabled && status == ExposeListenerState::Listening;
    Arc::new(Self {
      audit_state: Arc::downgrade(state),
      control: watch::channel(Control {
        rule: Arc::new(rule),
        accepting,
        terminate: false,
        deadline: None,
      })
      .0,
      status: Mutex::new((status, None)),
      sessions: Mutex::new(HashMap::new()),
      history: Mutex::new(VecDeque::new()),
      budget: Mutex::new(Budget::default()),
      opened: AtomicU64::new(0),
      up_bytes: AtomicU64::new(0),
      down_bytes: AtomicU64::new(0),
      up_packets: AtomicU64::new(0),
      down_packets: AtomicU64::new(0),
      drops: Mutex::new(BTreeMap::new()),
    })
  }
  pub fn dropped(&self, reason: &'static str) {
    *self
      .drops
      .lock()
      .unwrap_or_else(|e| e.into_inner())
      .entry(reason)
      .or_default() += 1;
  }
  pub fn status(&self, status: ExposeListenerState, error: Option<String>) {
    let mut stored = self.status.lock().unwrap_or_else(|e| e.into_inner());
    if *stored == (status, error.clone()) {
      return;
    }
    let before = stored.0;
    *stored = (status, error.clone());
    drop(stored);
    if let Some(state) = self.audit_state.upgrade() {
      let rule = self.control.borrow().rule.resource.clone();
      tokio::spawn(async move {
        let org = (rule.spec.org_id != "master").then(|| rule.spec.org_id.clone());
        state.audit_in("expose_runtime", "system", "system", org,
          &serde_json::json!({"id":rule.id,"revision":rule.revision,"before":before,"after":status,"error":error}).to_string()).await;
      });
    }
  }
  pub fn stop(&self, drain: Option<Duration>) {
    self.control.send_modify(|c| {
      c.accepting = false;
      c.terminate = drain.is_none();
      c.deadline = drain.map(|d| Instant::now() + d);
    });
    if drain.is_none() {
      for session in self
        .sessions
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
      {
        session.stop.send_replace(true);
      }
    }
    self.status(
      if drain.is_some() {
        ExposeListenerState::Draining
      } else {
        ExposeListenerState::Disabled
      },
      None,
    );
  }
  pub fn session_views(&self, history: bool) -> Vec<SessionView> {
    let mut views: Vec<_> = self
      .sessions
      .lock()
      .unwrap_or_else(|e| e.into_inner())
      .values()
      .map(|s| {
        let mut view = s.view.clone();
        view.idle_seconds = s.touched.elapsed().as_secs();
        view
      })
      .collect();
    if history {
      views.extend(
        self
          .history
          .lock()
          .unwrap_or_else(|e| e.into_inner())
          .iter()
          .cloned(),
      );
    }
    views
  }
  pub fn transfer(&self, id: &str, down: bool, bytes: usize) {
    let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = sessions.get_mut(id) {
      s.touched = Instant::now();
      if down {
        s.view.down_bytes += bytes as u64;
        s.view.down_packets += 1;
      } else {
        s.view.up_bytes += bytes as u64;
        s.view.up_packets += 1;
      }
    }
    if down {
      self.down_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
      self.down_packets.fetch_add(1, Ordering::Relaxed);
    } else {
      self.up_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
      self.up_packets.fetch_add(1, Ordering::Relaxed);
    }
  }
}

struct Entry {
  rule: ManagedRule,
  runtime: Arc<Runtime>,
  task: Option<tokio::task::JoinHandle<()>>,
}

enum BoundSocket {
  Tcp(tokio::net::TcpListener),
  Udp(Arc<tokio::net::UdpSocket>),
}

impl BoundSocket {
  fn bind(spec: &ExposeListener) -> Result<Self, String> {
    let address = SocketAddr::new(spec.address, spec.port);
    let domain = if address.is_ipv6() {
      socket2::Domain::IPV6
    } else {
      socket2::Domain::IPV4
    };
    let kind = match spec.protocol {
      ExposeProtocol::Tcp => socket2::Type::STREAM,
      ExposeProtocol::Udp => socket2::Type::DGRAM,
    };
    let socket = socket2::Socket::new(domain, kind, None).map_err(|e| e.to_string())?;
    if address.is_ipv6() {
      socket.set_only_v6(true).map_err(|e| e.to_string())?;
    }
    socket.set_nonblocking(true).map_err(|e| e.to_string())?;
    socket
      .bind(&address.into())
      .map_err(|e| format!("cannot bind {address}: {e}"))?;
    match spec.protocol {
      ExposeProtocol::Tcp => {
        socket.listen(128).map_err(|e| e.to_string())?;
        tokio::net::TcpListener::from_std(socket.into())
          .map(Self::Tcp)
          .map_err(|e| e.to_string())
      }
      ExposeProtocol::Udp => tokio::net::UdpSocket::from_std(socket.into())
        .map(|s| Self::Udp(Arc::new(s)))
        .map_err(|e| e.to_string()),
    }
  }
}

#[derive(Serialize)]
pub(crate) struct ExposeView {
  #[serde(flatten)]
  pub resource: ExposeResource,
  pub state: ExposeListenerState,
  pub target_state: ExposeTargetState,
  pub served_by: Option<String>,
  pub error: Option<String>,
  pub sessions: usize,
  pub up_bytes: u64,
  pub down_bytes: u64,
  pub up_packets: u64,
  pub down_packets: u64,
  pub drops: BTreeMap<&'static str, u64>,
}

#[derive(Default)]
struct RelayTotals {
  opened: u64,
  up_bytes: u64,
  down_bytes: u64,
  up_packets: u64,
  down_packets: u64,
  drops: BTreeMap<&'static str, u64>,
}
impl RelayTotals {
  fn add_runtime(&mut self, runtime: &Runtime) {
    self.opened = self
      .opened
      .saturating_add(runtime.opened.load(Ordering::Relaxed));
    self.up_bytes = self
      .up_bytes
      .saturating_add(runtime.up_bytes.load(Ordering::Relaxed));
    self.down_bytes = self
      .down_bytes
      .saturating_add(runtime.down_bytes.load(Ordering::Relaxed));
    self.up_packets = self
      .up_packets
      .saturating_add(runtime.up_packets.load(Ordering::Relaxed));
    self.down_packets = self
      .down_packets
      .saturating_add(runtime.down_packets.load(Ordering::Relaxed));
    for (&reason, &count) in runtime
      .drops
      .lock()
      .unwrap_or_else(|e| e.into_inner())
      .iter()
    {
      let value = self.drops.entry(reason).or_default();
      *value = value.saturating_add(count);
    }
  }
}

pub(crate) struct ExposeManager {
  pub store: ExposeStore,
  entries: BTreeMap<String, Entry>,
  retired: Vec<Entry>,
  retired_totals: BTreeMap<&'static str, RelayTotals>,
  pub file_error: Option<String>,
}

type StagedListeners = (HashMap<String, BoundSocket>, HashMap<String, String>);

impl ExposeManager {
  pub fn load(data_dir: &str) -> Self {
    Self {
      store: ExposeStore::load(data_dir),
      entries: BTreeMap::new(),
      retired: Vec::new(),
      retired_totals: BTreeMap::new(),
      file_error: None,
    }
  }

  pub fn resource(&self, id: &str) -> Option<&ExposeResource> {
    self.entries.get(id).map(|e| &e.rule.resource)
  }
  pub fn policy(&self, org: &str) -> Option<&ExposePolicy> {
    self
      .store
      .document
      .policies
      .iter()
      .find(|p| p.org_id == org)
  }
  pub fn sessions(&self, history: bool) -> Vec<SessionView> {
    let mut sessions: BTreeMap<String, SessionView> = if history {
      self
        .store
        .session_history()
        .into_iter()
        .map(|s| (s.id.clone(), s))
        .collect()
    } else {
      BTreeMap::new()
    };
    for view in self
      .entries
      .values()
      .chain(self.retired.iter())
      .flat_map(|e| e.runtime.session_views(history))
    {
      sessions.insert(view.id.clone(), view);
    }
    sessions.into_values().collect()
  }
  pub fn disconnect(&self, id: &str, session_id: &str) -> bool {
    for entry in self
      .entries
      .values()
      .chain(self.retired.iter())
      .filter(|e| e.rule.resource.id == id)
    {
      let sessions = entry
        .runtime
        .sessions
        .lock()
        .unwrap_or_else(|e| e.into_inner());
      if let Some(session) = sessions.get(session_id) {
        session.stop.send_replace(true);
        return true;
      }
    }
    false
  }
  pub fn drain(&mut self, id: &str, duration: Duration) -> Result<(), String> {
    let entry = self.entries.get(id).ok_or("unknown expose")?;
    if entry.rule.resource.source == ExposeSource::File {
      return Err("file rules must be edited in the config file".into());
    }
    let mut document = self.store.document.clone();
    let row = document
      .resources
      .iter_mut()
      .find(|r| r.id == id)
      .ok_or("unknown expose")?;
    row.spec.enabled = false;
    row.revision += 1;
    let saved = row.clone();
    self
      .store
      .commit(document)
      .map_err(|error| format!("persistence: {error}"))?;
    let entry = self
      .entries
      .get_mut(id)
      .expect("entry is locked by manager");
    entry.rule.resource = saved;
    entry
      .runtime
      .control
      .send_modify(|c| c.rule = Arc::new(entry.rule.clone()));
    entry.runtime.stop(Some(duration));
    Ok(())
  }

  pub async fn views(&mut self, state: &Arc<AppState>) -> Vec<ExposeView> {
    let mut views = Vec::new();
    for entry in self.entries.values() {
      let (status, error) = entry
        .runtime
        .status
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
      let (target_state, served_by) = match resolve::target(state, &entry.rule).await {
        Ok(target) => (ExposeTargetState::Ready, Some(target.client_id)),
        Err(reason) => (reason, None),
      };
      views.push(ExposeView {
        resource: entry.rule.resource.clone(),
        state: status,
        target_state,
        served_by,
        error,
        sessions: entry
          .runtime
          .sessions
          .lock()
          .unwrap_or_else(|e| e.into_inner())
          .len(),
        up_bytes: entry.runtime.up_bytes.load(Ordering::Relaxed),
        down_bytes: entry.runtime.down_bytes.load(Ordering::Relaxed),
        up_packets: entry.runtime.up_packets.load(Ordering::Relaxed),
        down_packets: entry.runtime.down_packets.load(Ordering::Relaxed),
        drops: entry
          .runtime
          .drops
          .lock()
          .unwrap_or_else(|e| e.into_inner())
          .clone(),
      });
    }
    views
  }

  fn file_rules(&self) -> Vec<ManagedRule> {
    self
      .entries
      .values()
      .filter(|e| e.rule.resource.source == ExposeSource::File)
      .map(|e| e.rule.clone())
      .collect()
  }

  pub async fn initialize(&mut self, state: &Arc<AppState>, host: &str) {
    // Invalid YAML must not erase otherwise valid API-owned desired state.
    let files = match self.read_file(state, host).await {
      Ok(rules) => {
        self.file_error = None;
        rules
      }
      Err(error) => {
        self.file_error = Some(error);
        Vec::new()
      }
    };
    let mut all = files.clone();
    all.extend(
      self
        .store
        .document
        .resources
        .iter()
        .cloned()
        .map(|resource| ManagedRule {
          resource,
          legacy: None,
        }),
    );
    for rule in all {
      let mut status = if rule.resource.spec.enabled {
        ExposeListenerState::Listening
      } else {
        ExposeListenerState::Disabled
      };
      let mut error = self
        .allowed(&rule.resource, &self.store.document, &files)
        .err();
      let mut socket = None;
      if error.is_some() {
        status = ExposeListenerState::Suspended;
      } else if rule.resource.spec.enabled {
        match BoundSocket::bind(&rule.resource.spec.listener) {
          Ok(bound) => socket = Some(bound),
          Err(why) => {
            status = ExposeListenerState::Failed;
            error = Some(why);
          }
        }
      }
      let runtime = Runtime::new(rule.clone(), status, state);
      runtime.status(status, error);
      let task = socket.map(|s| spawn(state, runtime.clone(), s));
      self.entries.insert(
        rule.resource.id.clone(),
        Entry {
          rule,
          runtime,
          task,
        },
      );
    }
  }

  async fn read_file(
    &mut self,
    state: &Arc<AppState>,
    host: &str,
  ) -> Result<Vec<ManagedRule>, String> {
    let entries: Vec<aperio_config::ExposeEntry> = match crate::config_file::structured("expose") {
      Some(value) => {
        serde_yaml::from_value(value).map_err(|e| format!("invalid expose section: {e}"))?
      }
      None => Vec::new(),
    };
    let errors = validate_entries(&entries, &[ExposeProtocol::Tcp, ExposeProtocol::Udp]);
    if !errors.is_empty() {
      return Err(
        errors
          .iter()
          .map(ToString::to_string)
          .collect::<Vec<_>>()
          .join(", "),
      );
    }
    let default_host: IpAddr = host
      .parse()
      .map_err(|_| "expose listener host must be an IP address")?;
    let mut rules = Vec::new();
    for entry in entries {
      let protocol = if entry.protocol == "udp" {
        ExposeProtocol::Udp
      } else {
        ExposeProtocol::Tcp
      };
      let org_id = match entry.explicit_org() {
        Some(org) => crate::tunnel::registry::org_id_for_name(state, org)
          .await
          .unwrap_or_else(|_| Some(format!("unresolved:{org}")))
          .unwrap_or_else(|| "master".into()),
        None => "master".into(),
      };
      let address = entry.address.unwrap_or(default_host);
      let id = format!("file:{}:{address}:{}", protocol.as_str(), entry.port);
      let spec = ExposeSpec {
        advertised_host: entry.advertised_host.clone(),
        org_id,
        tunnel: split_qualified(entry.tunnel.as_deref().unwrap_or("legacy_key"))
          .1
          .to_string(),
        listener: ExposeListener {
          address,
          port: entry.port,
          protocol,
        },
        enabled: entry.enabled,
        limits: entry
          .limits
          .clone()
          .unwrap_or_else(|| ExposeLimits::defaults(protocol)),
        allowed_ips: entry.allowed_ips.clone(),
      };
      let errors = spec.validate();
      if !errors.is_empty() {
        return Err(
          errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", "),
        );
      }
      let revision = self.entries.get(&id).map_or(1, |old| {
        old.rule.resource.revision
          + u64::from(old.rule.resource.spec != spec || old.rule.legacy.as_ref() != Some(&entry))
      });
      rules.push(ManagedRule {
        resource: ExposeResource {
          id,
          revision,
          source: ExposeSource::File,
          spec,
        },
        legacy: Some(entry),
      });
    }
    Ok(rules)
  }

  fn allowed(
    &self,
    resource: &ExposeResource,
    document: &ExposeDocument,
    files: &[ManagedRule],
  ) -> Result<(), String> {
    if resource.source == ExposeSource::File {
      return Ok(());
    }
    let policy = document
      .policies
      .iter()
      .find(|p| p.org_id == resource.spec.org_id);
    if policy.is_none() && resource.spec.org_id == "master" {
      return Ok(());
    }
    let policy = policy.ok_or("no server expose allocation for this organization")?;
    if !policy.allows(&resource.spec) {
      return Err("listener is outside the organization's port allocation".into());
    }
    if !policy.fits(
      document
        .resources
        .iter()
        .map(|r| &r.spec)
        .chain(files.iter().map(|r| &r.resource.spec)),
    ) {
      return Err("organization expose quota exceeded".into());
    }
    Ok(())
  }

  pub async fn reload(&mut self, state: &Arc<AppState>, host: &str) -> Result<(), String> {
    let files = match self.read_file(state, host).await {
      Ok(files) => files,
      Err(error) => {
        self.file_error = Some(error.clone());
        return Err(error);
      }
    };
    let document = self.store.document.clone();
    let result = self.apply(state, document, files, false, false).await;
    self.file_error = result.as_ref().err().cloned();
    result
  }

  pub async fn replace(
    &mut self,
    state: &Arc<AppState>,
    document: ExposeDocument,
    allow_suspension: bool,
  ) -> Result<(), String> {
    self
      .apply(state, document, self.file_rules(), true, allow_suspension)
      .await
  }

  /// Probe every changed bind using the same checks as a real apply. Dropping
  /// staged sockets leaves desired state, counters and existing tasks untouched.
  pub fn preview(&self, document: &ExposeDocument) -> Result<(), String> {
    self.stage(document, &self.file_rules(), false).map(|_| ())
  }

  fn stage(
    &self,
    document: &ExposeDocument,
    files: &[ManagedRule],
    allow_suspension: bool,
  ) -> Result<StagedListeners, String> {
    document.validate()?;
    let mut all = files.to_vec();
    all.extend(
      document
        .resources
        .iter()
        .cloned()
        .map(|resource| ManagedRule {
          resource,
          legacy: None,
        }),
    );
    let mut errors = HashMap::new();
    for (index, rule) in all.iter().enumerate() {
      let resource = &rule.resource;
      if let Err(error) = self.allowed(resource, document, files) {
        // A suspended organization must not prevent an unrelated tenant
        // from changing its own rules. Revalidate every changed organization.
        let org = &resource.spec.org_id;
        let previous: Vec<_> = self
          .entries
          .values()
          .filter(|e| &e.rule.resource.spec.org_id == org)
          .map(|e| &e.rule.resource)
          .collect();
        let desired: Vec<_> = all
          .iter()
          .filter(|r| &r.resource.spec.org_id == org)
          .map(|r| &r.resource)
          .collect();
        let unchanged = previous.len() == desired.len()
          && desired.iter().all(|r| previous.contains(r))
          && self
            .store
            .document
            .policies
            .iter()
            .find(|p| &p.org_id == org)
            == document.policies.iter().find(|p| &p.org_id == org);
        if !allow_suspension && !unchanged {
          return Err(error);
        }
        errors.insert(resource.id.clone(), error);
      }
      if resource.spec.enabled
        && all[..index].iter().any(|other| {
          other.resource.spec.enabled
            && other
              .resource
              .spec
              .listener
              .conflicts_with(&resource.spec.listener)
        })
      {
        return Err("port conflicts with another expose rule".into());
      }
    }
    // Retired listeners keep their ports until the drain completes.
    let mut staged = HashMap::new();
    for rule in &all {
      if !rule.resource.spec.enabled || errors.contains_key(&rule.resource.id) {
        continue;
      }
      // Explicit retry bumps the revision; unchanged failed resources stay
      // visibly failed and do not block changes to unrelated public ports.
      if self.entries.get(&rule.resource.id).is_some_and(|e| {
        e.rule.resource == rule.resource
          && e.runtime.status.lock().unwrap_or_else(|e| e.into_inner()).0
            == ExposeListenerState::Failed
      }) {
        continue;
      }
      let reusable = self.entries.get(&rule.resource.id).is_some_and(|e| {
        e.rule.resource.spec.listener == rule.resource.spec.listener
          && e.task.as_ref().is_some_and(|t| !t.is_finished())
          && e.runtime.control.borrow().accepting
      });
      if !reusable {
        let socket = BoundSocket::bind(&rule.resource.spec.listener)?;
        staged.insert(rule.resource.id.clone(), socket);
      }
    }
    Ok((staged, errors))
  }

  async fn apply(
    &mut self,
    state: &Arc<AppState>,
    document: ExposeDocument,
    files: Vec<ManagedRule>,
    persist: bool,
    allow_suspension: bool,
  ) -> Result<(), String> {
    let (mut staged, mut errors) = self.stage(&document, &files, allow_suspension)?;
    self.prune_retired();
    let mut all = files;
    all.extend(
      document
        .resources
        .iter()
        .cloned()
        .map(|resource| ManagedRule {
          resource,
          legacy: None,
        }),
    );
    if persist {
      self
        .store
        .commit(document)
        .map_err(|error| format!("persistence: {error}"))?;
    }
    let mut old = std::mem::take(&mut self.entries);
    for rule in all {
      let id = rule.resource.id.clone();
      let status = if errors.contains_key(&id) {
        ExposeListenerState::Suspended
      } else if !rule.resource.spec.enabled {
        ExposeListenerState::Disabled
      } else {
        ExposeListenerState::Listening
      };
      if let Some(mut entry) = old.remove(&id) {
        if entry.rule.resource == rule.resource
          && entry.rule.legacy == rule.legacy
          && !staged.contains_key(&id)
          && !errors.contains_key(&id)
        {
          // In particular, do not turn an in-progress drain into an immediate
          // stop merely because somebody edited a different listener.
          self.entries.insert(id, entry);
          continue;
        }
        if status == ExposeListenerState::Listening && !staged.contains_key(&id) {
          // Existing sessions retain their original target, but resource
          // ceilings and source-policy changes must not retain an old budget.
          if entry.rule.resource.spec.limits != rule.resource.spec.limits
            || entry.rule.resource.spec.allowed_ips != rule.resource.spec.allowed_ips
          {
            for session in entry
              .runtime
              .sessions
              .lock()
              .unwrap_or_else(|e| e.into_inner())
              .values()
            {
              session.stop.send_replace(true);
            }
          }
          entry.rule = rule.clone();
          entry
            .runtime
            .control
            .send_modify(|c| c.rule = Arc::new(rule));
          self.entries.insert(id, entry);
          continue;
        }
        entry.runtime.stop(None);
        finish_task(&mut entry.task).await;
        self.retired.push(entry);
      }
      let runtime = Runtime::new(rule.clone(), status, state);
      runtime.status(status, errors.remove(&id));
      let task = staged
        .remove(&id)
        .map(|socket| spawn(state, runtime.clone(), socket));
      self.entries.insert(
        id,
        Entry {
          rule,
          runtime,
          task,
        },
      );
    }
    for (_, mut entry) in old {
      entry.runtime.stop(None);
      finish_task(&mut entry.task).await;
      self.retired.push(entry);
    }
    Ok(())
  }

  fn prune_retired(&mut self) {
    let mut retained = Vec::new();
    for entry in self.retired.drain(..) {
      if entry.task.as_ref().is_some_and(|t| !t.is_finished())
        || !entry.runtime.session_views(false).is_empty()
      {
        retained.push(entry);
      } else {
        self
          .retired_totals
          .entry(entry.rule.resource.spec.listener.protocol.as_str())
          .or_default()
          .add_runtime(&entry.runtime);
      }
    }
    self.retired = retained;
  }

  /// Only protocol, state and fixed drop reasons become metric labels. No
  /// resource UUID, peer, tunnel name or tenant input creates a time series.
  pub fn render_metrics(&mut self, out: &mut String) {
    self.prune_retired();
    for (name, kind, help) in [
      (
        "aperio_expose_sessions_opened_total",
        "counter",
        "Public sessions admitted since process start",
      ),
      (
        "aperio_expose_bytes_total",
        "counter",
        "Public bytes forwarded since process start",
      ),
      (
        "aperio_expose_frames_total",
        "counter",
        "Public frames forwarded; UDP frames are datagrams",
      ),
      (
        "aperio_expose_drops_total",
        "counter",
        "Public relay rejections by fixed reason",
      ),
      (
        "aperio_expose_sessions",
        "gauge",
        "Current public relay sessions",
      ),
      (
        "aperio_expose_listeners",
        "gauge",
        "Current public resources by observed listener state",
      ),
    ] {
      out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} {kind}\n"));
    }
    for protocol in [ExposeProtocol::Tcp, ExposeProtocol::Udp] {
      let name = protocol.as_str();
      let mut total = RelayTotals::default();
      if let Some(saved) = self.retired_totals.get(name) {
        total.opened = saved.opened;
        total.up_bytes = saved.up_bytes;
        total.down_bytes = saved.down_bytes;
        total.up_packets = saved.up_packets;
        total.down_packets = saved.down_packets;
        total.drops = saved.drops.clone();
      }
      let mut sessions = 0;
      for entry in self
        .entries
        .values()
        .chain(self.retired.iter())
        .filter(|e| e.rule.resource.spec.listener.protocol == protocol)
      {
        total.add_runtime(&entry.runtime);
        sessions += entry
          .runtime
          .sessions
          .lock()
          .unwrap_or_else(|e| e.into_inner())
          .len();
      }
      out.push_str(&format!("aperio_expose_sessions_opened_total{{protocol=\"{name}\"}} {}\naperio_expose_sessions{{protocol=\"{name}\"}} {sessions}\n", total.opened));
      for (direction, bytes, frames) in [
        ("ingress", total.up_bytes, total.up_packets),
        ("egress", total.down_bytes, total.down_packets),
      ] {
        out.push_str(&format!("aperio_expose_bytes_total{{protocol=\"{name}\",direction=\"{direction}\"}} {bytes}\naperio_expose_frames_total{{protocol=\"{name}\",direction=\"{direction}\"}} {frames}\n"));
      }
      for (reason, count) in total.drops {
        out.push_str(&format!(
          "aperio_expose_drops_total{{protocol=\"{name}\",reason=\"{reason}\"}} {count}\n"
        ));
      }
      for (status, label) in [
        (ExposeListenerState::Disabled, "disabled"),
        (ExposeListenerState::Binding, "binding"),
        (ExposeListenerState::Listening, "listening"),
        (ExposeListenerState::Draining, "draining"),
        (ExposeListenerState::Suspended, "suspended"),
        (ExposeListenerState::Failed, "failed"),
      ] {
        let count = self
          .entries
          .values()
          .filter(|e| {
            e.rule.resource.spec.listener.protocol == protocol
              && e.runtime.status.lock().unwrap_or_else(|e| e.into_inner()).0 == status
          })
          .count();
        out.push_str(&format!(
          "aperio_expose_listeners{{protocol=\"{name}\",state=\"{label}\"}} {count}\n"
        ));
      }
    }
  }

  pub async fn shutdown(&mut self) {
    for entry in self.entries.values().chain(self.retired.iter()) {
      entry.runtime.stop(None);
    }
    for entry in self.entries.values_mut().chain(self.retired.iter_mut()) {
      finish_task(&mut entry.task).await;
    }
  }
}

/// Cancelling a supervisor must cancel its child too; dropping JoinHandle
/// alone would detach a live socket, allowing disable to report false success.
struct OwnedTask<T>(tokio::task::JoinHandle<T>);
impl<T> Drop for OwnedTask<T> {
  fn drop(&mut self) {
    self.0.abort();
  }
}

async fn finish_task(task: &mut Option<tokio::task::JoinHandle<()>>) {
  if let Some(mut task) = task.take()
    && tokio::time::timeout(Duration::from_secs(6), &mut task)
      .await
      .is_err()
  {
    task.abort();
    let _ = task.await;
  }
}

fn spawn(
  state: &Arc<AppState>,
  runtime: Arc<Runtime>,
  socket: BoundSocket,
) -> tokio::task::JoinHandle<()> {
  let state = Arc::downgrade(state);
  tokio::spawn(async move {
    let mut socket = Some(socket);
    let mut control = runtime.control.subscribe();
    for attempt in 0..=3u32 {
      let current = control.borrow().clone();
      if current.terminate
        || !current.accepting
        || state.upgrade().is_none_or(|s| *s.shutdown.borrow())
      {
        return;
      }
      let bound = match socket
        .take()
        .map(Ok)
        .unwrap_or_else(|| BoundSocket::bind(&current.rule.resource.spec.listener))
      {
        Ok(socket) => socket,
        Err(error) => {
          runtime.status(
            ExposeListenerState::Failed,
            Some(format!("recovery bind failed: {error}")),
          );
          if attempt == 3 {
            break;
          }
          tokio::select! { _ = tokio::time::sleep(Duration::from_secs(1 << attempt)) => {}, _ = control.changed() => {} }
          continue;
        }
      };
      runtime.status(ExposeListenerState::Listening, None);
      let mut inner = OwnedTask(tokio::spawn(relay::listen(
        state.clone(),
        runtime.clone(),
        bound,
      )));
      let result = (&mut inner.0).await;
      let current = control.borrow().clone();
      let status = runtime.status.lock().unwrap_or_else(|e| e.into_inner()).0;
      if current.terminate
        || !current.accepting
        || status == ExposeListenerState::Suspended
        || state.upgrade().is_none_or(|s| *s.shutdown.borrow())
      {
        return;
      }
      let message = match result {
        Err(error) => format!("listener task failed: {error}"),
        Ok(()) => "listener exited unexpectedly".into(),
      };
      runtime.status(
        ExposeListenerState::Failed,
        Some(format!("{message}; recovery attempt {attempt}/3")),
      );
      for session in runtime
        .sessions
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
      {
        session.stop.send_replace(true);
      }
      if attempt < 3 {
        tokio::select! {
          _ = tokio::time::sleep(Duration::from_secs(1 << attempt)) => {},
          _ = control.changed() => {},
        }
      }
    }
    runtime.status(
      ExposeListenerState::Failed,
      Some("listener recovery exhausted; retry from management API".into()),
    );
  })
}

#[cfg(test)]
#[path = "expose_manager/tests.rs"]
mod tests;
