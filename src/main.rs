use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use zellij_tile::prelude::*;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
struct AgentRecord {
    key: String,
    kind: String,
    codex_session_id: String,
    parent_key: String,
    zellij_session: String,
    pane_id: Option<u32>,
    attachment_id: String,
    cwd: String,
    project: String,
    project_root: String,
    title: String,
    status: String,
    unread: bool,
    dismissed: bool,
    message: String,
    model: String,
    branch: String,
    dirty: bool,
    pr: String,
    ports: Vec<u16>,
    updated_at: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
enum InputMode {
    #[default]
    Browse,
    Search,
    Reply,
    ConfirmReply,
    Title,
    WorktreeBranch,
    WorktreePrompt,
    ConfirmPark,
}

#[derive(Clone, Debug, PartialEq)]
enum JumpAction {
    MarkRead { key: String },
    HideDeck,
    FocusTerminalPane { pane_id: u32 },
    SwitchSession { session: String, pane_id: u32 },
}

#[derive(Clone, Debug, PartialEq)]
struct DetachRequest {
    key: String,
    attachment_id: String,
}

fn is_attached(agent: &AgentRecord) -> bool {
    agent.pane_id.is_some() && !agent.zellij_session.is_empty()
}

fn is_resumable(agent: &AgentRecord) -> bool {
    !is_attached(agent) && agent.kind == "codex" && !agent.codex_session_id.is_empty()
}

fn detach_requests_for_closed_pane(
    current_session: &str,
    pane_id: u32,
    agents: &[AgentRecord],
) -> Vec<DetachRequest> {
    agents
        .iter()
        .filter(|agent| {
            agent.zellij_session == current_session
                && agent.pane_id == Some(pane_id)
                && !agent.attachment_id.is_empty()
        })
        .map(|agent| DetachRequest {
            key: agent.key.clone(),
            attachment_id: agent.attachment_id.clone(),
        })
        .collect()
}

fn jump_plan(current_session: &str, agent: &AgentRecord) -> Result<Vec<JumpAction>, &'static str> {
    let pane_id = agent
        .pane_id
        .ok_or("This agent no longer has a live pane; press R to resume it")?;
    if agent.zellij_session.is_empty() {
        return Err("This agent no longer has a live pane; press R to resume it");
    }
    let navigate = if agent.zellij_session == current_session {
        JumpAction::FocusTerminalPane { pane_id }
    } else {
        JumpAction::SwitchSession {
            session: agent.zellij_session.clone(),
            pane_id,
        }
    };
    Ok(vec![
        JumpAction::MarkRead {
            key: agent.key.clone(),
        },
        JumpAction::HideDeck,
        navigate,
    ])
}

#[derive(Default)]
struct DeckModel {
    agents: Vec<AgentRecord>,
    selected: usize,
    filter: usize,
    query: String,
    show_subagents: bool,
    viewport_start: usize,
    viewport_len: usize,
}

impl DeckModel {
    fn includes_kind(&self, agent: &AgentRecord) -> bool {
        !agent.dismissed && (self.show_subagents || agent.kind != "subagent")
    }

    fn matches_filter(&self, agent: &AgentRecord) -> bool {
        if !self.includes_kind(agent) {
            return false;
        }
        match self.filter {
            1 => is_attached(agent) && agent.unread,
            2 => is_attached(agent) && (agent.status == "working" || agent.status == "idle"),
            3 => is_attached(agent) && agent.status == "needs_input",
            4 => is_attached(agent) && agent.status == "done",
            5 => is_attached(agent) && agent.status == "parked",
            6 => is_resumable(agent),
            _ => is_attached(agent),
        }
    }

    fn matching_indices(&self, live_query: Option<&str>) -> Vec<usize> {
        let query = live_query.unwrap_or(&self.query).to_lowercase();
        let included = self
            .agents
            .iter()
            .enumerate()
            .filter(|(_, agent)| {
                self.matches_filter(agent)
                    && (query.is_empty()
                        || format!(
                            "{} {} {} {}",
                            agent.project, agent.title, agent.branch, agent.message
                        )
                        .to_lowercase()
                        .contains(&query))
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if !self.show_subagents {
            return included;
        }

        let mut ordered = Vec::with_capacity(included.len());
        let mut emitted = vec![false; self.agents.len()];
        for parent_index in included.iter().copied() {
            let parent = &self.agents[parent_index];
            if parent.kind == "subagent" {
                continue;
            }
            ordered.push(parent_index);
            emitted[parent_index] = true;
            for child_index in included.iter().copied().filter(|child_index| {
                let child = &self.agents[*child_index];
                child.kind == "subagent" && child.parent_key == parent.key
            }) {
                ordered.push(child_index);
                emitted[child_index] = true;
            }
        }
        for index in included {
            if !emitted[index] {
                ordered.push(index);
            }
        }
        ordered
    }

    fn selected_agent(&self, live_query: Option<&str>) -> Option<AgentRecord> {
        self.matching_indices(live_query)
            .get(self.selected)
            .and_then(|index| self.agents.get(*index))
            .cloned()
    }

    fn clamp_selection(&mut self, live_query: Option<&str>) {
        let len = self.matching_indices(live_query).len();
        self.selected = self.selected.min(len.saturating_sub(1));
    }

    fn move_selection(&mut self, delta: isize, live_query: Option<&str>) {
        let len = self.matching_indices(live_query).len();
        if len == 0 {
            self.selected = 0;
        } else {
            self.selected = (self.selected as isize + delta).rem_euclid(len as isize) as usize;
        }
    }

    fn set_filter(&mut self, filter: usize) {
        self.filter = filter;
        self.selected = 0;
    }

    fn set_query(&mut self, query: String) {
        self.query = query;
        self.selected = 0;
    }

    fn clear_query(&mut self) {
        self.set_query(String::new());
    }

    fn toggle_subagents(&mut self) {
        self.show_subagents = !self.show_subagents;
        self.selected = 0;
    }

    fn update_viewport(&mut self, list_height: usize, matching_len: usize) {
        self.viewport_start = self.selected.saturating_sub(list_height.saturating_sub(1));
        self.viewport_len = matching_len
            .saturating_sub(self.viewport_start)
            .min(list_height);
    }

    fn select_viewport_row(&mut self, row: usize) -> bool {
        if row >= self.viewport_len {
            return false;
        }
        self.selected = self.viewport_start + row;
        true
    }
}

#[derive(Default)]
struct AgentDeck {
    helper: String,
    current_session: String,
    model: DeckModel,
    mode: InputMode,
    input: String,
    staged: String,
    notice: String,
    visible: bool,
    permissions_granted: bool,
    refresh_ticks: u8,
    next_list_request: u64,
    applied_list_request: u64,
}

const FILTERS: [&str; 7] = [
    "live", "unread", "running", "waiting", "done", "parked", "resume",
];

impl AgentDeck {
    fn required_permissions() -> [PermissionType; 4] {
        [
            PermissionType::ReadApplicationState,
            PermissionType::ChangeApplicationState,
            PermissionType::RunCommands,
            PermissionType::ReadSessionEnvironmentVariables,
        ]
    }

    fn subscribed_events() -> [EventType; 7] {
        [
            EventType::Key,
            EventType::Mouse,
            EventType::Visible,
            EventType::Timer,
            EventType::RunCommandResult,
            EventType::PermissionRequestResult,
            EventType::PaneClosed,
        ]
    }

    fn context(operation: &str) -> BTreeMap<String, String> {
        BTreeMap::from([("operation".to_owned(), operation.to_owned())])
    }

    fn run_helper(&self, operation: &str, args: &[String]) {
        self.run_helper_with_context(args, Self::context(operation));
    }

    fn run_helper_with_context(&self, args: &[String], context: BTreeMap<String, String>) {
        if !self.permissions_granted {
            return;
        }
        let mut command = vec![self.helper.clone()];
        command.extend(args.iter().cloned());
        let refs = command.iter().map(String::as_str).collect::<Vec<_>>();
        run_command(&refs, context);
    }

    fn refresh(&mut self, enrich: bool, reconcile: bool) {
        if !self.permissions_granted {
            return;
        }
        let mut args = vec!["list".to_owned()];
        if enrich {
            args.push("--refresh".to_owned());
        }
        if reconcile {
            args.push("--reconcile".to_owned());
        }
        self.next_list_request = self.next_list_request.wrapping_add(1);
        let mut context = Self::context("list");
        context.insert("request_id".into(), self.next_list_request.to_string());
        self.run_helper_with_context(&args, context);
    }

    fn matching_indices(&self) -> Vec<usize> {
        let live_query = if self.mode == InputMode::Search {
            Some(self.input.as_str())
        } else {
            None
        };
        self.model.matching_indices(live_query)
    }

    fn selected_agent(&self) -> Option<AgentRecord> {
        let live_query = (self.mode == InputMode::Search).then_some(self.input.as_str());
        self.model.selected_agent(live_query)
    }

    fn clamp_selection(&mut self) {
        let live_query = (self.mode == InputMode::Search).then_some(self.input.clone());
        self.model.clamp_selection(live_query.as_deref());
    }

    fn move_selection(&mut self, delta: isize) {
        let live_query = (self.mode == InputMode::Search).then_some(self.input.clone());
        self.model.move_selection(delta, live_query.as_deref());
    }

    fn set_input_mode(&mut self, mode: InputMode, prompt: &str) {
        self.mode = mode;
        self.input.clear();
        self.notice = prompt.to_owned();
    }

    fn cancel_input(&mut self) {
        self.mode = InputMode::Browse;
        self.input.clear();
        self.staged.clear();
        self.notice.clear();
    }

    fn mutate_selected(&mut self, command: &str, extra: &[String]) {
        if let Some(agent) = self.selected_agent() {
            let mut args = vec![command.to_owned(), agent.key];
            args.extend(extra.iter().cloned());
            self.run_helper(command, &args);
            self.notice = format!("{} requested", command);
        }
    }

    fn jump_selected(&mut self) {
        if let Some(agent) = self.selected_agent() {
            match jump_plan(&self.current_session, &agent) {
                Ok(actions) => {
                    for action in actions {
                        match action {
                            JumpAction::MarkRead { key } => {
                                self.run_helper("mark-read", &["mark-read".into(), key]);
                            }
                            JumpAction::HideDeck => hide_self(),
                            JumpAction::FocusTerminalPane { pane_id } => {
                                focus_terminal_pane(pane_id, false, false);
                            }
                            JumpAction::SwitchSession { session, pane_id } => {
                                switch_session_with_focus(&session, None, Some((pane_id, false)));
                            }
                        }
                    }
                }
                Err(message) => self.notice = message.into(),
            }
        }
    }

    fn apply_agent_signal(&mut self, agent: AgentRecord) {
        if agent.dismissed {
            self.model
                .agents
                .retain(|existing| existing.key != agent.key);
            self.clamp_selection();
            if self.permissions_granted {
                self.refresh(false, false);
                self.applied_list_request = self.next_list_request;
            }
            return;
        }
        if let Some(existing) = self
            .model
            .agents
            .iter_mut()
            .find(|existing| existing.key == agent.key)
        {
            *existing = agent.clone();
        } else {
            self.model.agents.push(agent.clone());
        }
        self.clamp_selection();
        if self.permissions_granted {
            self.sync_agent_pane(&agent);
            self.refresh(false, false);
            self.applied_list_request = self.next_list_request;
        }
    }

    fn sync_agent_pane(&self, agent: &AgentRecord) {
        if agent.zellij_session != self.current_session {
            return;
        }
        if let Some(pane_id) = agent.pane_id {
            let pane = PaneId::Terminal(pane_id);
            let wants_attention =
                agent.unread && matches!(agent.status.as_str(), "needs_input" | "done");
            if wants_attention {
                highlight_and_unhighlight_panes(vec![pane], vec![]);
            } else {
                highlight_and_unhighlight_panes(vec![], vec![pane]);
            }
            let label = truncate(&format!("{}: {}", agent.project, agent.title), 80);
            rename_terminal_pane(pane_id, label);
        }
    }

    fn handle_closed_pane(&mut self, pane_id: u32) {
        let requests =
            detach_requests_for_closed_pane(&self.current_session, pane_id, &self.model.agents);
        for request in &requests {
            self.run_helper(
                "detach-pane",
                &[
                    "detach-pane".into(),
                    request.key.clone(),
                    request.attachment_id.clone(),
                ],
            );
        }
        for agent in &mut self.model.agents {
            if requests.iter().any(|request| {
                request.key == agent.key && request.attachment_id == agent.attachment_id
            }) {
                agent.pane_id = None;
                agent.attachment_id.clear();
                if agent.status != "parked" {
                    agent.status = "ended".into();
                }
                agent.unread = false;
            }
        }
        self.clamp_selection();
    }

    fn activate_after_permissions_granted(&mut self) {
        self.permissions_granted = true;
        self.current_session = get_session_environment_variables()
            .remove("ZELLIJ_SESSION_NAME")
            .unwrap_or_default();
        for agent in &self.model.agents {
            self.sync_agent_pane(agent);
        }
        set_timeout(15.0);
        self.refresh(false, true);
        hide_self();
    }

    fn submit_input(&mut self) {
        let value = self.input.trim().to_owned();
        match self.mode {
            InputMode::Search => {
                self.model.set_query(value.clone());
                self.mode = InputMode::Browse;
                self.notice = if value.is_empty() {
                    String::new()
                } else {
                    format!("filter: {value}")
                };
            }
            InputMode::Reply if !value.is_empty() => {
                self.staged = value;
                self.mode = InputMode::ConfirmReply;
                self.notice = "Send this reply? y/n".into();
            }
            InputMode::Title if !value.is_empty() => {
                self.mutate_selected("title", &[value]);
                self.cancel_input();
            }
            InputMode::WorktreeBranch if !value.is_empty() => {
                self.staged = value;
                self.set_input_mode(
                    InputMode::WorktreePrompt,
                    "Optional first Codex prompt (Enter to skip)",
                );
            }
            InputMode::WorktreePrompt => {
                if let Some(agent) = self.selected_agent() {
                    self.run_helper(
                        "worktree",
                        &["worktree".into(), agent.key, self.staged.clone(), value],
                    );
                    self.cancel_input();
                    self.notice = "Creating worktree and Codex pane…".into();
                }
            }
            _ => {}
        }
        self.input.clear();
    }

    fn handle_key(&mut self, key: KeyWithModifier) {
        let bare = key.bare_key;
        match self.mode {
            InputMode::ConfirmReply => match bare {
                BareKey::Char('y') | BareKey::Char('Y') => {
                    let message = self.staged.clone();
                    self.mutate_selected("reply", &[message]);
                    self.cancel_input();
                }
                BareKey::Char('n') | BareKey::Char('N') | BareKey::Esc => self.cancel_input(),
                _ => {}
            },
            InputMode::ConfirmPark => match bare {
                BareKey::Char('y') | BareKey::Char('Y') => {
                    self.mutate_selected("park", &[]);
                    self.cancel_input();
                }
                BareKey::Char('n') | BareKey::Char('N') | BareKey::Esc => self.cancel_input(),
                _ => {}
            },
            InputMode::Browse => match bare {
                BareKey::Esc | BareKey::Char('q') => hide_self(),
                BareKey::Down | BareKey::Char('j') => self.move_selection(1),
                BareKey::Up | BareKey::Char('k') => self.move_selection(-1),
                BareKey::Enter => self.jump_selected(),
                BareKey::Char('/') => self.set_input_mode(InputMode::Search, "Search agents"),
                BareKey::Char('r') => {
                    self.set_input_mode(InputMode::Reply, "Reply to selected agent")
                }
                BareKey::Char('t') => self.set_input_mode(InputMode::Title, "Set task title"),
                BareKey::Char('w') => {
                    self.set_input_mode(InputMode::WorktreeBranch, "New worktree branch")
                }
                BareKey::Char('p') => {
                    self.mode = InputMode::ConfirmPark;
                    self.notice = "Park selected agent with Ctrl-C? y/n".into();
                }
                BareKey::Char('R') => {
                    let fallback_session = self.current_session.clone();
                    self.mutate_selected("resume", &[fallback_session]);
                }
                BareKey::Char('m') => self.mutate_selected("mark-read", &[]),
                BareKey::Char('d') => self.mutate_selected("dismiss", &[]),
                BareKey::Char('g') => {
                    self.notice = "Refreshing git, PR, and port metadata…".into();
                    self.refresh(true, true);
                }
                BareKey::Char('c') => {
                    self.model.clear_query();
                    self.notice.clear();
                }
                BareKey::Char('s') => {
                    self.model.toggle_subagents();
                    self.notice = format!(
                        "Subagents {}",
                        if self.model.show_subagents {
                            "shown"
                        } else {
                            "hidden"
                        }
                    );
                }
                BareKey::Char(ch @ '1'..='7') => {
                    self.model.set_filter(ch as usize - '1' as usize);
                }
                _ => {}
            },
            _ => match bare {
                BareKey::Esc => self.cancel_input(),
                BareKey::Enter => self.submit_input(),
                BareKey::Backspace => {
                    self.input.pop();
                }
                BareKey::Char(ch) if !key.key_modifiers.contains(&KeyModifier::Ctrl) => {
                    self.input.push(ch)
                }
                _ => {}
            },
        }
    }

    fn handle_result(
        &mut self,
        code: Option<i32>,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        context: BTreeMap<String, String>,
    ) {
        let operation = context.get("operation").map(String::as_str).unwrap_or("");
        if operation == "list" && code.unwrap_or(1) == 0 {
            let request_id = context
                .get("request_id")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            if request_id < self.applied_list_request {
                return;
            }
            match serde_json::from_slice::<Vec<AgentRecord>>(&stdout) {
                Ok(agents) => {
                    self.applied_list_request = request_id;
                    self.model.agents = agents;
                    self.clamp_selection();
                    if self.notice.starts_with("Refreshing") {
                        self.notice = "Metadata refreshed".into();
                    }
                }
                Err(error) => self.notice = format!("Could not read agent state: {error}"),
            }
        } else if operation != "list" {
            if code.unwrap_or(1) == 0 {
                self.notice = format!("{operation} complete");
                self.refresh(false, false);
            } else {
                let message = String::from_utf8_lossy(&stderr);
                self.notice = truncate(&format!("{operation} failed: {}", message.trim()), 120);
            }
        }
    }
}

impl ZellijPlugin for AgentDeck {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        self.helper = configuration
            .get("helper")
            .cloned()
            .unwrap_or_else(|| "zellij-agent-deck".into());
        self.model.show_subagents = configuration
            .get("show_subagents")
            .is_some_and(|value| parse_bool(value));
        subscribe(&Self::subscribed_events());
        set_selectable(true);
        request_permission(&Self::required_permissions());
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::Key(key) => {
                self.handle_key(key);
                return true;
            }
            Event::Mouse(Mouse::ScrollDown(_)) => {
                self.move_selection(1);
                return true;
            }
            Event::Mouse(Mouse::ScrollUp(_)) => {
                self.move_selection(-1);
                return true;
            }
            Event::Mouse(Mouse::LeftClick(line, _)) if line >= 3 => {
                return self
                    .model
                    .select_viewport_row((line as usize).saturating_sub(3));
            }
            Event::Visible(visible) => {
                self.visible = visible;
                if visible {
                    self.refresh(false, true);
                }
            }
            Event::Timer(_) => {
                self.refresh_ticks = self.refresh_ticks.wrapping_add(1);
                self.refresh(false, self.refresh_ticks.is_multiple_of(10));
                set_timeout(if self.visible { 3.0 } else { 15.0 });
            }
            Event::PaneClosed(PaneId::Terminal(pane_id)) => {
                self.handle_closed_pane(pane_id);
                return true;
            }
            Event::RunCommandResult(code, stdout, stderr, context) => {
                self.handle_result(code, stdout, stderr, context);
                return true;
            }
            Event::PermissionRequestResult(PermissionStatus::Denied) => {
                self.permissions_granted = false;
                self.notice = "Agent Deck permissions were denied".into();
            }
            Event::PermissionRequestResult(PermissionStatus::Granted) => {
                self.activate_after_permissions_granted();
            }
            _ => return false,
        }
        self.visible
    }

    fn pipe(&mut self, pipe_message: PipeMessage) -> bool {
        if pipe_message.name == "agent-event" {
            if let Some(payload) = pipe_message.payload {
                if let Ok(agent) = serde_json::from_str::<AgentRecord>(&payload) {
                    self.apply_agent_signal(agent);
                    return true;
                }
            }
        } else if pipe_message.name == "toggle" {
            show_self(true);
        }
        self.visible
    }

    fn render(&mut self, rows: usize, cols: usize) {
        let width = cols.saturating_sub(2);
        let live = self
            .model
            .agents
            .iter()
            .filter(|agent| self.model.includes_kind(agent) && is_attached(agent))
            .count();
        let resumable = self
            .model
            .agents
            .iter()
            .filter(|agent| is_resumable(agent))
            .count();
        let unread = self
            .model
            .agents
            .iter()
            .filter(|agent| self.model.includes_kind(agent) && is_attached(agent) && agent.unread)
            .count();
        let waiting = self
            .model
            .agents
            .iter()
            .filter(|agent| {
                self.model.includes_kind(agent)
                    && is_attached(agent)
                    && agent.status == "needs_input"
            })
            .count();
        let header = truncate(
            &format!(
                " Agent Deck  {} live · {} resume · {} unread · {} waiting",
                live, resumable, unread, waiting
            ),
            width,
        );
        print_text_with_coordinates(Text::new(header).color_all(3), 1, 0, Some(width), None);

        let filters = FILTERS
            .iter()
            .enumerate()
            .map(|(index, name)| {
                if self.model.filter == index {
                    format!("[{}:{}]", index + 1, name)
                } else {
                    format!(" {}:{} ", index + 1, name)
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        let filters = format!(
            "{} · subagents:{}",
            filters,
            if self.model.show_subagents {
                "on"
            } else {
                "off"
            }
        );
        print_text_with_coordinates(
            Text::new(truncate(&filters, width)).dim_all(),
            1,
            1,
            Some(width),
            None,
        );

        let matching = self.matching_indices();
        let list_height = rows.saturating_sub(8);
        self.model.update_viewport(list_height, matching.len());
        let scroll = self.model.viewport_start;
        for (screen_index, agent_index) in
            matching.iter().skip(scroll).take(list_height).enumerate()
        {
            let agent = &self.model.agents[*agent_index];
            let cursor = if scroll + screen_index == self.model.selected {
                "›"
            } else {
                " "
            };
            let unread_mark = if agent.unread { "●" } else { " " };
            let state = status_symbol(&agent.status);
            let position = scroll + screen_index;
            let label = if let Some(connector) =
                subagent_connector(&self.model.agents, &matching, position)
            {
                format!(
                    "{cursor}{unread_mark}{state}  {connector} subagent: {}",
                    agent.title
                )
            } else {
                format!(
                    "{cursor}{unread_mark}{state}  {}: {}",
                    agent.project, agent.title
                )
            };
            let text = Text::new(truncate(&label, width));
            let text = if scroll + screen_index == self.model.selected {
                text.color_all(3)
            } else {
                text
            };
            print_text_with_coordinates(text, 1, 3 + screen_index, Some(width), None);
        }

        if let Some(agent) = self.selected_agent() {
            let detail_y = rows.saturating_sub(4);
            let dirty = if agent.dirty { "*" } else { "" };
            let ports = if agent.ports.is_empty() {
                String::new()
            } else {
                format!(
                    " ports:{}",
                    agent
                        .ports
                        .iter()
                        .map(u16::to_string)
                        .collect::<Vec<_>>()
                        .join(",")
                )
            };
            let pr = if agent.pr.is_empty() {
                String::new()
            } else {
                format!(" {}", agent.pr)
            };
            let detail = format!(
                " {} · {}{}{}{} · {}",
                agent.zellij_session, agent.branch, dirty, pr, ports, agent.status
            );
            print_text_with_coordinates(
                Text::new(truncate(&detail, width)).dim_all(),
                1,
                detail_y,
                Some(width),
                None,
            );
            if !agent.message.is_empty() {
                print_text_with_coordinates(
                    Text::new(truncate(&format!(" {}", agent.message), width)),
                    1,
                    detail_y + 1,
                    Some(width),
                    None,
                );
            }
        }

        let prompt_y = rows.saturating_sub(2);
        let prompt = if matches!(
            self.mode,
            InputMode::Browse | InputMode::ConfirmReply | InputMode::ConfirmPark
        ) {
            self.notice.clone()
        } else {
            format!("{}: {}_", self.notice, self.input)
        };
        print_text_with_coordinates(
            Text::new(truncate(&prompt, width)),
            1,
            prompt_y,
            Some(width),
            None,
        );
        let keys = " Enter jump · r reply · t title · w worktree · p park · R resume · m read · d dismiss · g refresh · s subagents · / search · q close ";
        print_text_with_coordinates(
            Text::new(truncate(keys, width)).dim_all(),
            1,
            rows.saturating_sub(1),
            Some(width),
            None,
        );
    }
}

fn status_symbol(status: &str) -> &'static str {
    match status {
        "working" => "◐",
        "needs_input" => "!",
        "done" => "✓",
        "parked" => "Ⅱ",
        "ended" => "×",
        _ => "○",
    }
}

fn parse_bool(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "on" | "true" | "yes"
    )
}

fn subagent_connector(
    agents: &[AgentRecord],
    matching: &[usize],
    position: usize,
) -> Option<&'static str> {
    let agent = matching
        .get(position)
        .and_then(|index| agents.get(*index))?;
    if agent.kind != "subagent" {
        return None;
    }
    let has_next_sibling = matching
        .get(position + 1)
        .and_then(|index| agents.get(*index))
        .is_some_and(|next| next.kind == "subagent" && next.parent_key == agent.parent_key);
    Some(if has_next_sibling { "├─" } else { "└─" })
}

fn truncate(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_owned();
    }
    if limit <= 1 {
        return "…".chars().take(limit).collect();
    }
    let mut result = value.chars().take(limit - 1).collect::<String>();
    result.push('…');
    result
}

register_plugin!(AgentDeck);

#[cfg(test)]
mod tests {
    use super::*;

    // The Zellij SDK imports this host function even when a unit test does not
    // exercise a host command. Native tests provide a no-op implementation so
    // the test binary can link outside the WASM host.
    #[no_mangle]
    extern "C" fn host_run_plugin_command() {}

    #[test]
    fn startup_requires_session_environment_permission() {
        assert!(AgentDeck::required_permissions()
            .contains(&PermissionType::ReadSessionEnvironmentVariables));
        assert!(!AgentDeck::default().permissions_granted);
    }

    #[test]
    fn truncates_on_character_boundaries() {
        assert_eq!(truncate("example-project", 6), "examp…");
        assert_eq!(truncate("سلام", 3), "سل…");
    }

    #[test]
    fn status_symbols_are_distinct() {
        assert_ne!(status_symbol("working"), status_symbol("needs_input"));
        assert_ne!(status_symbol("done"), status_symbol("parked"));
    }

    #[test]
    fn plugin_boolean_configuration_is_explicit_and_default_safe() {
        for enabled in ["true", "TRUE", "1", "yes", "on"] {
            assert!(parse_bool(enabled));
        }
        for disabled in ["false", "0", "no", "off", "unexpected", ""] {
            assert!(!parse_bool(disabled));
        }
    }

    #[test]
    fn handled_key_requests_redraw_when_visibility_event_was_missed() {
        let mut deck = AgentDeck::default();

        assert!(deck.update(Event::Key(KeyWithModifier::new(BareKey::Down))));
    }

    #[test]
    fn completed_dismiss_requests_redraw_when_visibility_event_was_missed() {
        let mut deck = AgentDeck::default();
        let context = AgentDeck::context("dismiss");

        assert!(deck.update(Event::RunCommandResult(
            Some(0),
            Vec::new(),
            Vec::new(),
            context,
        )));
    }

    #[test]
    fn new_agent_event_requests_redraw_when_visibility_event_was_missed() {
        let mut deck = AgentDeck::default();
        let payload = serde_json::to_string(&AgentRecord {
            key: "codex:new".into(),
            project: "example".into(),
            title: "new task".into(),
            ..Default::default()
        })
        .unwrap();
        let message = PipeMessage::new(
            PipeSource::Cli("test".into()),
            "agent-event",
            &Some(payload),
            &None,
            false,
        );

        assert!(deck.pipe(message));
        assert_eq!(deck.model.agents.len(), 1);
    }

    #[test]
    fn dismissed_agent_signal_removes_an_existing_agent_immediately() {
        let mut deck = AgentDeck {
            model: DeckModel {
                agents: vec![AgentRecord {
                    key: "codex:internal".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        };

        deck.apply_agent_signal(AgentRecord {
            key: "codex:internal".into(),
            dismissed: true,
            ..Default::default()
        });

        assert!(deck.model.agents.is_empty());
    }

    #[test]
    fn jump_within_current_session_focuses_the_terminal_pane() {
        let agent = AgentRecord {
            key: "codex:example".into(),
            zellij_session: "work".into(),
            pane_id: Some(7),
            ..Default::default()
        };

        assert_eq!(
            jump_plan("work", &agent),
            Ok(vec![
                JumpAction::MarkRead {
                    key: "codex:example".into(),
                },
                JumpAction::HideDeck,
                JumpAction::FocusTerminalPane { pane_id: 7 },
            ])
        );
    }

    #[test]
    fn stale_list_result_cannot_restore_a_dismissed_agent() {
        let mut deck = AgentDeck::default();
        let mut latest = AgentDeck::context("list");
        latest.insert("request_id".into(), "2".into());
        let mut stale = AgentDeck::context("list");
        stale.insert("request_id".into(), "1".into());
        let dismissed = serde_json::to_vec(&vec![AgentRecord {
            key: "codex:dismissed".into(),
            ..Default::default()
        }])
        .unwrap();

        deck.handle_result(Some(0), b"[]".to_vec(), Vec::new(), latest);
        deck.handle_result(Some(0), dismissed, Vec::new(), stale);

        assert!(deck.model.agents.is_empty());
    }

    #[test]
    fn jump_hides_deck_before_switching_to_terminal_pane() {
        let agent = AgentRecord {
            key: "codex:example".into(),
            zellij_session: "work".into(),
            pane_id: Some(7),
            ..Default::default()
        };

        assert_eq!(
            jump_plan("deck", &agent),
            Ok(vec![
                JumpAction::MarkRead {
                    key: "codex:example".into(),
                },
                JumpAction::HideDeck,
                JumpAction::SwitchSession {
                    session: "work".into(),
                    pane_id: 7,
                },
            ])
        );
    }

    #[test]
    fn live_filter_hides_detached_sessions_and_resume_filter_restores_them() {
        let mut deck = AgentDeck {
            model: DeckModel {
                agents: vec![
                    AgentRecord {
                        key: "codex:live".into(),
                        zellij_session: "work".into(),
                        pane_id: Some(7),
                        ..Default::default()
                    },
                    AgentRecord {
                        key: "codex:resume".into(),
                        kind: "codex".into(),
                        codex_session_id: "session-id".into(),
                        pane_id: None,
                        ..Default::default()
                    },
                    AgentRecord {
                        key: "subagent:hidden".into(),
                        kind: "subagent".into(),
                        codex_session_id: "session-id".into(),
                        pane_id: None,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            ..Default::default()
        };

        assert_eq!(deck.matching_indices(), vec![0]);
        deck.model.set_filter(6);
        assert_eq!(deck.matching_indices(), vec![1]);
    }

    #[test]
    fn subagents_are_hidden_by_default_and_nested_when_enabled() {
        let mut model = DeckModel {
            agents: vec![
                AgentRecord {
                    key: "subagent:parent:first".into(),
                    kind: "subagent".into(),
                    parent_key: "codex:parent".into(),
                    zellij_session: "work".into(),
                    pane_id: Some(7),
                    ..Default::default()
                },
                AgentRecord {
                    key: "codex:parent".into(),
                    zellij_session: "work".into(),
                    pane_id: Some(7),
                    ..Default::default()
                },
                AgentRecord {
                    key: "subagent:parent:last".into(),
                    kind: "subagent".into(),
                    parent_key: "codex:parent".into(),
                    zellij_session: "work".into(),
                    pane_id: Some(7),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        assert_eq!(model.matching_indices(None), vec![1]);

        model.toggle_subagents();
        let matching = model.matching_indices(None);
        assert_eq!(matching, vec![1, 0, 2]);
        assert_eq!(subagent_connector(&model.agents, &matching, 0), None);
        assert_eq!(subagent_connector(&model.agents, &matching, 1), Some("├─"));
        assert_eq!(subagent_connector(&model.agents, &matching, 2), Some("└─"));
    }

    #[test]
    fn subagent_key_toggles_visibility() {
        let mut deck = AgentDeck::default();

        deck.handle_key(KeyWithModifier::new(BareKey::Char('s')));

        assert!(deck.model.show_subagents);
        assert_eq!(deck.notice, "Subagents shown");
    }

    #[test]
    fn clicking_a_scrolled_row_selects_its_viewport_item() {
        let mut model = DeckModel {
            agents: (0..10)
                .map(|pane_id| AgentRecord {
                    key: format!("codex:{pane_id}"),
                    zellij_session: "work".into(),
                    pane_id: Some(pane_id),
                    ..Default::default()
                })
                .collect(),
            selected: 7,
            ..Default::default()
        };

        model.update_viewport(3, model.matching_indices(None).len());

        assert!(model.select_viewport_row(1));
        assert_eq!(model.selected_agent(None).unwrap().key, "codex:6");
        assert!(!model.select_viewport_row(3));
    }

    #[test]
    fn pane_close_detaches_only_the_matching_attachment_generation() {
        let agents = vec![
            AgentRecord {
                key: "codex:closed".into(),
                zellij_session: "work".into(),
                pane_id: Some(7),
                attachment_id: "generation-a".into(),
                ..Default::default()
            },
            AgentRecord {
                key: "codex:other-session".into(),
                zellij_session: "other".into(),
                pane_id: Some(7),
                attachment_id: "generation-b".into(),
                ..Default::default()
            },
        ];

        assert_eq!(
            detach_requests_for_closed_pane("work", 7, &agents),
            vec![DetachRequest {
                key: "codex:closed".into(),
                attachment_id: "generation-a".into(),
            }]
        );
    }

    #[test]
    fn plugin_subscribes_to_pane_close_events() {
        assert!(AgentDeck::subscribed_events().contains(&EventType::PaneClosed));
    }
}
