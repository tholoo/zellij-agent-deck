use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use unicode_width::UnicodeWidthStr;
use zellij_tile::prelude::*;

mod ui;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
struct AgentRecord {
    key: String,
    kind: String,
    codex_session_id: String,
    opencode_session_id: String,
    parent_key: String,
    zellij_session: String,
    pane_id: Option<u32>,
    attachment_id: String,
    cwd: String,
    project: String,
    project_root: String,
    repository_root: String,
    worktree: String,
    title: String,
    status: String,
    unread: bool,
    dismissed: bool,
    message: String,
    activity: String,
    model: String,
    branch: String,
    dirty: bool,
    pr: String,
    ports: Vec<u16>,
    updated_at: u64,
    status_since: u64,
    activity_since: u64,
    attention_seq: u64,
    revision: u64,
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
    Help,
    Details,
    WorktreePick,
    WorktreeSearch,
    ConfirmWorktree,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct WorktreeInfo {
    path: String,
    branch: String,
    head: String,
    current: bool,
    locked: bool,
    prunable: bool,
    agents: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct WorktreePlan {
    path: String,
    branch: String,
    base_branch: String,
    base_head: String,
    existing: bool,
}

#[derive(Clone, Debug, PartialEq)]
enum JumpAction {
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
    !is_attached(agent)
        && match agent.kind.as_str() {
            "codex" => !agent.codex_session_id.is_empty(),
            "opencode" => !agent.opencode_session_id.is_empty(),
            _ => false,
        }
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
    Ok(vec![JumpAction::HideDeck, navigate])
}

#[derive(Default)]
struct DeckModel {
    agents: Vec<AgentRecord>,
    selected: usize,
    filter: usize,
    query: String,
    show_subagents: bool,
    viewport_start: usize,
    show_inactive: bool,
    group_by_project: bool,
    worktree_scope: Option<String>,
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
            2 => is_attached(agent) && agent.status == "working",
            3 => is_attached(agent) && needs_attention(agent),
            4 => is_attached(agent) && agent.status == "done",
            5 => is_attached(agent) && agent.status == "parked",
            6 => is_resumable(agent),
            _ => is_attached(agent),
        }
    }

    fn matching_indices(&self, live_query: Option<&str>) -> Vec<usize> {
        let query = live_query.unwrap_or(&self.query).to_lowercase();
        let mut included = self
            .agents
            .iter()
            .enumerate()
            .filter(|(_, agent)| {
                self.matches_filter(agent)
                    && self
                        .worktree_scope
                        .as_ref()
                        .is_none_or(|path| agent.project_root == *path)
                    && (self.filter != 0
                        || self.show_inactive
                        || !query.is_empty()
                        || group_rank(agent) < 3)
                    && (query.is_empty()
                        || format!(
                            "{} {} {} {} {} {} {} {}",
                            agent.project,
                            agent.title,
                            agent.branch,
                            agent.message,
                            agent.activity,
                            agent.cwd,
                            agent.worktree,
                            agent.zellij_session
                        )
                        .to_lowercase()
                        .contains(&query))
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        included.sort_by(|left, right| {
            let a = &self.agents[*left];
            let b = &self.agents[*right];
            let order = if self.group_by_project {
                a.repository_root
                    .cmp(&b.repository_root)
                    .then(a.project.cmp(&b.project))
                    .then(group_rank(a).cmp(&group_rank(b)))
            } else {
                group_rank(a).cmp(&group_rank(b))
            };
            order
                .then(b.status_since.cmp(&a.status_since))
                .then(a.key.cmp(&b.key))
        });
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

    fn restore_selection(&mut self, key: Option<&str>, live_query: Option<&str>) {
        if let Some(position) = self
            .matching_indices(live_query)
            .iter()
            .position(|index| Some(self.agents[*index].key.as_str()) == key)
        {
            self.selected = position;
        }
        self.clamp_selection(live_query);
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
        self.worktree_scope = None;
        self.set_query(String::new());
    }

    fn toggle_subagents(&mut self) {
        self.show_subagents = !self.show_subagents;
        self.selected = 0;
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
    focus_request: Option<Vec<AgentRecord>>,
    acknowledged: BTreeMap<String, (String, u64)>,
    read_save_failures: BTreeMap<String, String>,
    action_target: Option<AgentRecord>,
    screen_hits: BTreeMap<usize, ui::Hit>,
    compact_status: bool,
    last_attention_key: String,
    pending_attention: bool,
    worktrees: Vec<WorktreeInfo>,
    worktree_selected: usize,
    worktree_query: String,
    worktree_plan: Option<WorktreePlan>,
    worktree_prompt: String,
    worktree_busy: bool,
    dialog_id: u64,
    detail_scroll: usize,
    plugin_id: Option<u32>,
    manual_size: bool,
}

impl AgentDeck {
    fn required_permissions() -> [PermissionType; 4] {
        [
            PermissionType::ReadApplicationState,
            PermissionType::ChangeApplicationState,
            PermissionType::RunCommands,
            PermissionType::ReadSessionEnvironmentVariables,
        ]
    }

    fn subscribed_events() -> [EventType; 11] {
        [
            EventType::Key,
            EventType::Mouse,
            EventType::Visible,
            EventType::Timer,
            EventType::RunCommandResult,
            EventType::PermissionRequestResult,
            EventType::PaneClosed,
            EventType::PaneUpdate,
            EventType::TabUpdate,
            EventType::SessionUpdate,
            EventType::ListClients,
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
        if !matches!(
            self.mode,
            InputMode::Browse | InputMode::Search | InputMode::Help | InputMode::Details
        ) {
            if let Some(target) = &self.action_target {
                return Some(target.clone());
            }
        }
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
        if self.action_target.is_none() {
            self.action_target = self.selected_agent();
        }
        self.mode = mode;
        self.input.clear();
        self.notice = prompt.to_owned();
    }

    fn cancel_input(&mut self) {
        self.mode = InputMode::Browse;
        self.input.clear();
        self.staged.clear();
        self.action_target = None;
        self.notice.clear();
        self.dialog_id = self.dialog_id.wrapping_add(1);
        self.worktree_busy = false;
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
        let selected_key = self.selected_agent().map(|agent| agent.key);
        if self
            .model
            .agents
            .iter()
            .any(|existing| existing.key == agent.key && existing.revision > agent.revision)
        {
            return;
        }
        let agent = self.merge_acknowledgement(agent);
        if agent.dismissed {
            self.model
                .agents
                .retain(|existing| existing.key != agent.key);
            self.model.restore_selection(
                selected_key.as_deref(),
                (self.mode == InputMode::Search).then_some(self.input.as_str()),
            );
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
        self.model.restore_selection(
            selected_key.as_deref(),
            (self.mode == InputMode::Search).then_some(self.input.as_str()),
        );
        self.request_focus();
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
                agent.unread && matches!(agent.status.as_str(), "needs_input" | "error" | "done");
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

    fn merge_acknowledgement(&self, mut agent: AgentRecord) -> AgentRecord {
        if self.acknowledged.get(&agent.key)
            == Some(&(agent.attachment_id.clone(), agent.attention_seq))
        {
            agent.unread = false;
        }
        agent
    }

    fn request_focus(&mut self) {
        if self.focus_request.is_some() {
            return;
        }
        let candidates = self
            .model
            .agents
            .iter()
            .filter(|agent| {
                agent.unread
                    && !agent.dismissed
                    && agent.kind != "subagent"
                    && agent.zellij_session == self.current_session
                    && is_attached(agent)
            })
            .cloned()
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return;
        }
        // Capture the result generations BEFORE querying focus. A completion
        // arriving while this request is in flight needs its own observation.
        self.focus_request = Some(candidates);
        if self.permissions_granted {
            list_clients();
        }
    }

    fn acknowledge_clients(&mut self, clients: Vec<ClientInfo>) {
        let selected_key = self.selected_agent().map(|agent| agent.key);
        let candidates = self.focus_request.take().unwrap_or_default();
        for candidate in candidates {
            if !clients
                .iter()
                .any(|client| Some(client.pane_id) == candidate.pane_id.map(PaneId::Terminal))
            {
                continue;
            }
            if let Some(agent) = self.model.agents.iter_mut().find(|agent| {
                agent.key == candidate.key
                    && agent.unread
                    && agent.attachment_id == candidate.attachment_id
                    && agent.attention_seq == candidate.attention_seq
            }) {
                agent.unread = false;
                self.acknowledged.insert(
                    agent.key.clone(),
                    (agent.attachment_id.clone(), agent.attention_seq),
                );
                let agent = agent.clone();
                let mut context = Self::context("auto-read");
                context.insert("key".into(), agent.key.clone());
                self.run_helper_with_context(
                    &[
                        "mark-read".into(),
                        agent.key.clone(),
                        "--attention-seq".into(),
                        agent.attention_seq.to_string(),
                        "--attachment-id".into(),
                        agent.attachment_id.clone(),
                    ],
                    context,
                );
                if self.permissions_granted {
                    self.sync_agent_pane(&agent);
                }
            }
        }
        // Any result received during the query gets a fresh focus observation.
        // Don't repeatedly query unread background panes here.
        self.model.restore_selection(
            selected_key.as_deref(),
            (self.mode == InputMode::Search).then_some(self.input.as_str()),
        );
    }

    fn jump_next_attention(&mut self) {
        let mut candidates = self
            .model
            .agents
            .iter()
            .filter(|agent| {
                self.model.includes_kind(agent)
                    && is_attached(agent)
                    && (needs_attention(agent) || (agent.status == "done" && agent.unread))
            })
            .cloned()
            .collect::<Vec<_>>();
        candidates.sort_by(|a, b| group_rank(a).cmp(&group_rank(b)).then(a.key.cmp(&b.key)));
        let next = candidates
            .iter()
            .position(|agent| agent.key == self.last_attention_key)
            .map(|position| (position + 1) % candidates.len())
            .unwrap_or(0);
        if let Some(agent) = candidates.get(next) {
            self.last_attention_key = agent.key.clone();
            self.model.filter = 0;
            self.model.query.clear();
            self.model.worktree_scope = None;
            self.model.restore_selection(Some(&agent.key), None);
            self.jump_selected();
        } else {
            self.notice = "Nothing needs your attention".into();
        }
    }

    fn worktree_indices(&self) -> Vec<usize> {
        let query = if self.mode == InputMode::WorktreeSearch {
            &self.input
        } else {
            &self.worktree_query
        }
        .to_lowercase();
        self.worktrees
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                format!("{} {}", item.branch, item.path)
                    .to_lowercase()
                    .contains(&query)
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn worktree_command(&mut self, operation: &str, args: &[String]) {
        let mut context = Self::context(operation);
        context.insert("dialog_id".into(), self.dialog_id.to_string());
        self.worktree_busy = true;
        self.run_helper_with_context(args, context);
    }

    fn browse_worktrees(&mut self) {
        let Some(agent) = self.selected_agent() else {
            self.notice = "Select a session to browse its repository's worktrees".into();
            return;
        };
        self.action_target = Some(agent.clone());
        self.dialog_id = self.dialog_id.wrapping_add(1);
        self.worktrees.clear();
        self.worktree_query.clear();
        self.worktree_selected = 0;
        self.worktree_plan = None;
        self.mode = InputMode::WorktreePick;
        self.notice = "Loading worktrees…".into();
        self.worktree_command("worktrees", &["worktrees".into(), agent.key]);
    }

    fn open_selected_worktree(&mut self) {
        let Some(index) = self.worktree_indices().get(self.worktree_selected).copied() else {
            return;
        };
        let item = self.worktrees[index].clone();
        if item.prunable {
            self.notice = "This checkout is missing; refresh after repairing it with Git".into();
            return;
        }
        let agents = self
            .model
            .agents
            .iter()
            .filter(|agent| item.agents.contains(&agent.key) && is_attached(agent))
            .cloned()
            .collect::<Vec<_>>();
        if !agents.is_empty() {
            self.cancel_input();
            self.model.filter = 0;
            self.model.show_inactive = true;
            self.model.set_query(String::new());
            self.model.worktree_scope = Some(item.path);
            self.model.restore_selection(Some(&agents[0].key), None);
            if agents.len() == 1 {
                self.jump_selected();
            } else {
                self.notice = format!("{} agents in this worktree · Enter to open", agents.len());
            }
            return;
        }
        self.worktree_plan = Some(WorktreePlan {
            path: item.path,
            branch: item.branch,
            base_head: item.head,
            existing: true,
            ..Default::default()
        });
        self.worktree_prompt.clear();
        self.mode = InputMode::ConfirmWorktree;
        self.notice = "Start an agent in this worktree? y/n".into();
    }

    fn confirm_worktree(&mut self) {
        let (Some(agent), Some(plan)) = (self.selected_agent(), self.worktree_plan.clone()) else {
            return;
        };
        if plan.existing {
            self.worktree_command(
                "open-worktree",
                &["open-worktree".into(), agent.key, plan.path],
            );
        } else {
            self.worktree_command(
                "worktree",
                &[
                    "worktree".into(),
                    agent.key,
                    plan.branch,
                    self.worktree_prompt.clone(),
                    "--base-head".into(),
                    plan.base_head,
                ],
            );
        }
        self.notice = "Opening agent in a new pane…".into();
    }

    fn activate_after_permissions_granted(&mut self) {
        self.permissions_granted = true;
        if let Some(plugin_id) = self.plugin_id {
            rename_plugin_pane(plugin_id, "Agent Deck");
        }
        self.current_session = get_session_environment_variables()
            .remove("ZELLIJ_SESSION_NAME")
            .unwrap_or_default();
        for agent in &self.model.agents {
            self.sync_agent_pane(agent);
        }
        set_timeout(3.0);
        self.refresh(false, true);
        if !self.compact_status {
            hide_self();
        }
    }

    fn read_save_notice(&self) -> String {
        self.read_save_failures.values().next().map_or_else(String::new, |detail| {
            if detail.contains("unrecognized arguments")
                && (detail.contains("--attention-seq") || detail.contains("--attachment-id"))
            {
                "Could not save read state: outdated helper. Refresh Zellij config and reload Deck.".into()
            } else {
                truncate(&format!("Could not save read state; will retry: {detail}"), 180)
            }
        })
    }

    fn open_deck(&self) {
        if !self.permissions_granted || self.compact_status {
            return;
        }
        let Some(plugin_id) = self.plugin_id else {
            return;
        };
        let Ok((target_tab, _)) = get_focused_pane_info() else {
            return;
        };
        // Prepare the floating pane without focusing it. LaunchOrFocusPlugin
        // resets geometry when moving tabs; BreakPanes preserves it instead.
        let pane_id = PaneId::Plugin(plugin_id);
        let coordinates = if self.manual_size {
            get_pane_info(pane_id).map_or_else(FloatingPaneCoordinates::default, |pane| {
                FloatingPaneCoordinates::default()
                    .with_x_fixed(pane.pane_x)
                    .with_y_fixed(pane.pane_y)
                    .with_width_fixed(pane.pane_columns)
                    .with_height_fixed(pane.pane_rows)
            })
        } else {
            FloatingPaneCoordinates::default()
                .with_x_percent(4)
                .with_y_percent(5)
                .with_width_percent(92)
                .with_height_percent(90)
        };
        change_floating_panes_coordinates(vec![(pane_id, coordinates)]);
        break_panes_to_tab_with_index(&[pane_id], target_tab, false);
        show_self(true);
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
                if !self.worktree_busy {
                    if let Some(agent) = self.selected_agent() {
                        self.worktree_command(
                            "worktree-plan",
                            &["worktree-plan".into(), agent.key, value],
                        );
                        self.notice = "Checking branch and destination…".into();
                    }
                }
                return;
            }
            InputMode::WorktreePrompt => {
                self.worktree_prompt = value;
                self.mode = InputMode::ConfirmWorktree;
                self.notice = "Create worktree and start agent? y/n".into();
            }
            InputMode::WorktreeSearch => {
                self.worktree_query = value;
                self.worktree_selected = 0;
                self.mode = InputMode::WorktreePick;
                self.notice.clear();
            }
            _ => {}
        }
        self.input.clear();
    }

    fn handle_key(&mut self, key: KeyWithModifier) {
        let bare = key.bare_key;
        match self.mode {
            InputMode::Details => match bare {
                BareKey::Esc | BareKey::Char('q') | BareKey::Tab => self.cancel_input(),
                BareKey::Down | BareKey::Char('j') => {
                    self.detail_scroll = self.detail_scroll.saturating_add(1)
                }
                BareKey::Up | BareKey::Char('k') => {
                    self.detail_scroll = self.detail_scroll.saturating_sub(1)
                }
                BareKey::PageDown => self.detail_scroll = self.detail_scroll.saturating_add(5),
                BareKey::PageUp => self.detail_scroll = self.detail_scroll.saturating_sub(5),
                _ => {}
            },
            InputMode::WorktreePick => match bare {
                BareKey::Esc | BareKey::Char('q') => self.cancel_input(),
                BareKey::Down | BareKey::Char('j') => {
                    self.worktree_selected = (self.worktree_selected + 1)
                        .min(self.worktree_indices().len().saturating_sub(1));
                }
                BareKey::Up | BareKey::Char('k') => {
                    self.worktree_selected = self.worktree_selected.saturating_sub(1)
                }
                BareKey::Char('/') => {
                    self.set_input_mode(InputMode::WorktreeSearch, "Find branch or path")
                }
                BareKey::Char('c') => {
                    self.worktree_query.clear();
                    self.worktree_selected = 0;
                }
                BareKey::Char('n') if !self.worktree_busy => {
                    self.worktree_plan = None;
                    self.set_input_mode(InputMode::WorktreeBranch, "New branch name")
                }
                BareKey::Char('g') if !self.worktree_busy => self.browse_worktrees(),
                BareKey::Enter if !self.worktree_busy => self.open_selected_worktree(),
                _ => {}
            },
            InputMode::ConfirmWorktree => match bare {
                BareKey::Char('y') | BareKey::Char('Y') if !self.worktree_busy => {
                    self.confirm_worktree()
                }
                BareKey::Char('n') | BareKey::Esc if !self.worktree_busy => {
                    self.mode = InputMode::WorktreePick;
                    self.notice.clear();
                }
                _ => {}
            },
            InputMode::Help => {
                if matches!(bare, BareKey::Esc | BareKey::Char('q') | BareKey::Char('?')) {
                    self.cancel_input();
                }
            }
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
                BareKey::Tab => {
                    self.detail_scroll = 0;
                    self.set_input_mode(InputMode::Details, "Session details");
                }
                BareKey::Char('?') => self.mode = InputMode::Help,
                BareKey::Char('n') => self.jump_next_attention(),
                BareKey::Char('i') => {
                    self.model.show_inactive = !self.model.show_inactive;
                    self.clamp_selection();
                }
                BareKey::Char('v') => {
                    let key = self.selected_agent().map(|agent| agent.key);
                    self.model.group_by_project = !self.model.group_by_project;
                    self.model.restore_selection(key.as_deref(), None);
                }
                BareKey::Char('/') => self.set_input_mode(InputMode::Search, "Search agents"),
                BareKey::Char('r') => {
                    self.set_input_mode(InputMode::Reply, "Reply to selected agent")
                }
                BareKey::Char('t') => self.set_input_mode(InputMode::Title, "Set task title"),
                BareKey::Char('w') => self.browse_worktrees(),
                BareKey::Char('p') => {
                    self.action_target = self.selected_agent();
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
                BareKey::Esc if self.mode == InputMode::WorktreeSearch => {
                    self.mode = InputMode::WorktreePick;
                    self.input.clear();
                    self.notice.clear();
                }
                BareKey::Esc if self.mode == InputMode::WorktreeBranch => {
                    self.dialog_id = self.dialog_id.wrapping_add(1);
                    self.worktree_busy = false;
                    self.mode = InputMode::WorktreePick;
                    self.notice.clear();
                }
                BareKey::Esc if self.mode == InputMode::WorktreePrompt => {
                    self.mode = InputMode::WorktreeBranch;
                    self.input = self
                        .worktree_plan
                        .as_ref()
                        .map(|p| p.branch.clone())
                        .unwrap_or_default();
                    self.worktree_plan = None;
                    self.notice = "New branch name".into();
                }
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
        if self.mode == InputMode::Search {
            self.clamp_selection();
        }
        if self.mode == InputMode::WorktreeSearch {
            self.worktree_selected = 0;
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
        if matches!(
            operation,
            "worktrees" | "worktree-plan" | "worktree" | "open-worktree"
        ) {
            if context
                .get("dialog_id")
                .and_then(|value| value.parse::<u64>().ok())
                != Some(self.dialog_id)
            {
                return;
            }
            self.worktree_busy = false;
            if code.unwrap_or(1) != 0 {
                self.notice = truncate(&String::from_utf8_lossy(&stderr), 300);
                return;
            }
            match operation {
                "worktrees" => match serde_json::from_slice::<Vec<WorktreeInfo>>(&stdout) {
                    Ok(items) => {
                        self.worktrees = items;
                        self.notice.clear();
                    }
                    Err(error) => self.notice = format!("Could not read worktrees: {error}"),
                },
                "worktree-plan" => match serde_json::from_slice::<WorktreePlan>(&stdout) {
                    Ok(plan) => {
                        self.worktree_plan = Some(plan);
                        self.set_input_mode(
                            InputMode::WorktreePrompt,
                            "Optional first prompt (Enter to skip)",
                        );
                    }
                    Err(error) => self.notice = format!("Could not read worktree preview: {error}"),
                },
                _ => {
                    self.cancel_input();
                    self.notice = "Agent opened in its worktree".into();
                    self.refresh(false, false);
                }
            }
            return;
        }
        if operation == "auto-read" {
            let previous_notice = self.read_save_notice();
            let failed = code.unwrap_or(1) != 0;
            if let Some(key) = context.get("key") {
                if failed {
                    self.acknowledged.remove(key);
                    let stderr = String::from_utf8_lossy(&stderr);
                    let detail = stderr.lines().rev().find(|line| !line.trim().is_empty());
                    self.read_save_failures.insert(
                        key.clone(),
                        detail
                            .unwrap_or("helper returned no error details")
                            .trim()
                            .into(),
                    );
                } else {
                    self.read_save_failures.remove(key);
                }
            }
            // A recovered save must not leave a stale warning behind, or erase
            // a newer notice from a user action. Other failed saves still matter.
            if failed || self.notice == previous_notice {
                self.notice = self.read_save_notice();
            }
            self.refresh(false, false);
            return;
        }
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
                    let selected_key = self.selected_agent().map(|agent| agent.key);
                    self.applied_list_request = request_id;
                    self.model.agents = agents
                        .into_iter()
                        .map(|agent| {
                            let newest = self
                                .model
                                .agents
                                .iter()
                                .find(|existing| {
                                    existing.key == agent.key && existing.revision > agent.revision
                                })
                                .cloned()
                                .unwrap_or(agent);
                            self.merge_acknowledgement(newest)
                        })
                        .collect();
                    let query = (self.mode == InputMode::Search).then_some(self.input.as_str());
                    self.model.restore_selection(selected_key.as_deref(), query);
                    self.request_focus();
                    if self.pending_attention {
                        self.pending_attention = false;
                        self.jump_next_attention();
                    }
                    if self.notice.starts_with("Refreshing") {
                        self.notice = "Metadata refreshed".into();
                    }
                }
                Err(error) => self.notice = format!("Could not read agent state: {error}"),
            }
        } else if operation == "list" {
            self.notice = truncate(
                &format!(
                    "Could not refresh sessions: {}",
                    String::from_utf8_lossy(&stderr).trim()
                ),
                180,
            );
        } else {
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
        self.plugin_id = Some(get_plugin_ids().plugin_id);
        self.manual_size = configuration
            .get("auto_size")
            .is_some_and(|value| !parse_bool(value));
        self.helper = configuration
            .get("helper")
            .cloned()
            .unwrap_or_else(|| "zellij-agent-deck".into());
        self.model.show_subagents = configuration
            .get("show_subagents")
            .is_some_and(|value| parse_bool(value));
        self.compact_status = configuration
            .get("display")
            .is_some_and(|value| value == "status");
        self.model.show_inactive = configuration
            .get("show_inactive")
            .is_some_and(|value| parse_bool(value));
        subscribe(&Self::subscribed_events());
        set_selectable(!self.compact_status);
        request_permission(&Self::required_permissions());
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::Key(key) => {
                self.handle_key(key);
                return true;
            }
            Event::Mouse(Mouse::ScrollDown(_)) => {
                if matches!(self.mode, InputMode::WorktreePick | InputMode::Details) {
                    self.handle_key(KeyWithModifier::new(BareKey::Down));
                } else if matches!(self.mode, InputMode::Browse | InputMode::Search) {
                    self.move_selection(1);
                }
                return true;
            }
            Event::Mouse(Mouse::ScrollUp(_)) => {
                if matches!(self.mode, InputMode::WorktreePick | InputMode::Details) {
                    self.handle_key(KeyWithModifier::new(BareKey::Up));
                } else if matches!(self.mode, InputMode::Browse | InputMode::Search) {
                    self.move_selection(-1);
                }
                return true;
            }
            Event::Mouse(Mouse::LeftClick(line, _)) if line >= 0 => {
                match self.screen_hits.get(&(line as usize)) {
                    Some(ui::Hit::Agent(position)) => self.model.selected = *position,
                    Some(ui::Hit::Inactive) => self.model.show_inactive = !self.model.show_inactive,
                    Some(ui::Hit::Worktree(position)) => self.worktree_selected = *position,
                    None => return false,
                }
                return true;
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
                set_timeout(3.0);
                self.request_focus();
            }
            Event::PaneUpdate(_) => {
                self.request_focus();
            }
            Event::TabUpdate(_) | Event::SessionUpdate(_, _) => {
                self.request_focus();
            }
            Event::ListClients(clients) => {
                self.acknowledge_clients(clients);
                return true;
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
        } else if matches!(pipe_message.name.as_str(), "open" | "toggle") {
            self.open_deck();
        } else if pipe_message.name == "attention-next" && !self.compact_status {
            self.pending_attention = true;
            self.refresh(false, false);
        }
        self.visible
    }

    fn render(&mut self, rows: usize, cols: usize) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let screen = ui::screen(self, rows, cols, now);
        self.screen_hits = screen.hits;
        for line in screen.lines {
            let mut text = Text::new(line.text);
            text = match line.style {
                ui::Style::Heading => text.color_all(1),
                ui::Style::Muted => text.unbold_all(),
                ui::Style::Selected => text.selected(),
                ui::Style::Alert => text.color_all(0),
                ui::Style::Normal => text,
            };
            print_text_with_coordinates(text, line.x, line.y, Some(line.width), None);
        }
    }
}

fn status_symbol(status: &str) -> &'static str {
    match status {
        "working" => "◐",
        "needs_input" | "error" => "!",
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

fn needs_attention(agent: &AgentRecord) -> bool {
    matches!(agent.status.as_str(), "needs_input" | "error")
}

fn group_rank(agent: &AgentRecord) -> u8 {
    if needs_attention(agent) {
        0
    } else if agent.status == "done" && agent.unread {
        1
    } else if agent.status == "working" {
        2
    } else {
        3
    }
}

fn truncate(value: &str, limit: usize) -> String {
    if value.width() <= limit {
        return value.to_owned();
    }
    if limit == 0 {
        return String::new();
    }
    let mut result = String::new();
    for ch in value.chars() {
        if result.width() + unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0) > limit - 1 {
            break;
        }
        result.push(ch);
    }
    result.push('…');
    result
}

register_plugin!(AgentDeck);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manually_visiting_an_agent_clears_unread_without_resolving_approval() {
        let mut deck = AgentDeck {
            current_session: "work".into(),
            model: DeckModel {
                agents: vec![AgentRecord {
                    key: "codex:visit".into(),
                    zellij_session: "work".into(),
                    pane_id: Some(7),
                    attachment_id: "first".into(),
                    status: "needs_input".into(),
                    unread: true,
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        };
        deck.update(Event::PaneUpdate(PaneManifest::default()));
        deck.update(Event::ListClients(vec![ClientInfo::new(
            1,
            PaneId::Terminal(7),
            "codex".into(),
            true,
        )]));
        assert!(
            !deck.model.agents[0].unread,
            "manual visit must clear unread"
        );
        assert_eq!(deck.model.agents[0].status, "needs_input");
    }

    fn unread_agent(key: &str, session: &str, pane_id: u32) -> AgentRecord {
        AgentRecord {
            key: key.into(),
            zellij_session: session.into(),
            pane_id: Some(pane_id),
            attachment_id: "first".into(),
            attention_seq: 1,
            revision: 1,
            status: "done".into(),
            unread: true,
            ..Default::default()
        }
    }

    #[test]
    fn background_tabs_other_sessions_and_detached_clients_do_not_count_as_seen() {
        let mut deck = AgentDeck {
            current_session: "work".into(),
            model: DeckModel {
                agents: vec![
                    unread_agent("background-tab", "work", 7),
                    unread_agent("other-session", "other", 8),
                ],
                ..Default::default()
            },
            ..Default::default()
        };
        deck.update(Event::TabUpdate(vec![]));
        deck.update(Event::ListClients(vec![ClientInfo::new(
            1,
            PaneId::Terminal(8),
            String::new(),
            true,
        )]));
        assert!(deck.model.agents.iter().all(|agent| agent.unread));
        deck.update(Event::TabUpdate(vec![]));
        deck.update(Event::ListClients(vec![]));
        assert!(deck.model.agents.iter().all(|agent| agent.unread));
    }

    #[test]
    fn stale_focus_observation_does_not_acknowledge_a_new_result() {
        let mut deck = AgentDeck {
            current_session: "work".into(),
            model: DeckModel {
                agents: vec![unread_agent("agent", "work", 7)],
                ..Default::default()
            },
            ..Default::default()
        };
        deck.update(Event::PaneUpdate(PaneManifest::default()));
        let mut next = deck.model.agents[0].clone();
        next.attention_seq = 2;
        next.revision = 2;
        deck.apply_agent_signal(next);
        deck.update(Event::ListClients(vec![ClientInfo::new(
            1,
            PaneId::Terminal(7),
            String::new(),
            true,
        )]));
        assert!(deck.model.agents[0].unread);
        deck.update(Event::PaneUpdate(PaneManifest::default()));
        deck.update(Event::ListClients(vec![ClientInfo::new(
            1,
            PaneId::Terminal(7),
            String::new(),
            true,
        )]));
        assert!(!deck.model.agents[0].unread);
    }

    #[test]
    fn stale_signals_and_list_snapshots_cannot_restore_acknowledged_unread() {
        let original = unread_agent("agent", "work", 7);
        let mut deck = AgentDeck {
            current_session: "work".into(),
            model: DeckModel {
                agents: vec![original.clone()],
                ..Default::default()
            },
            ..Default::default()
        };
        deck.request_focus();
        deck.acknowledge_clients(vec![ClientInfo::new(
            1,
            PaneId::Terminal(7),
            String::new(),
            true,
        )]);
        deck.apply_agent_signal(original.clone());
        deck.handle_result(
            Some(0),
            serde_json::to_vec(&vec![original]).unwrap(),
            vec![],
            AgentDeck::context("list"),
        );
        assert!(!deck.model.agents[0].unread);
    }

    #[test]
    fn recovered_read_save_clears_its_warning() {
        let mut deck = AgentDeck::default();
        let mut context = AgentDeck::context("auto-read");
        context.insert("key".into(), "codex:fixture".into());
        deck.handle_result(
            Some(1),
            vec![],
            b"temporary failure".to_vec(),
            context.clone(),
        );
        assert!(deck.notice.contains("Could not save read state"));
        deck.handle_result(Some(0), vec![], vec![], context);
        assert!(
            deck.notice.is_empty(),
            "a successful retry must clear the save warning"
        );
    }

    #[test]
    fn one_successful_read_does_not_hide_another_failed_save() {
        let mut deck = AgentDeck::default();
        let mut a = AgentDeck::context("auto-read");
        a.insert("key".into(), "a".into());
        let mut b = a.clone();
        b.insert("key".into(), "b".into());
        deck.handle_result(Some(1), vec![], b"save A failed".to_vec(), a.clone());
        deck.handle_result(Some(1), vec![], b"save B failed".to_vec(), b.clone());
        deck.handle_result(Some(0), vec![], vec![], a);
        assert!(deck.notice.contains("save B failed"));
        deck.notice = "Reply sent".into();
        deck.handle_result(Some(0), vec![], vec![], b);
        assert_eq!(deck.notice, "Reply sent");
    }

    #[test]
    fn old_helper_errors_explain_how_to_recover() {
        let mut deck = AgentDeck::default();
        let mut context = AgentDeck::context("auto-read");
        context.insert("key".into(), "a".into());
        deck.handle_result(
            Some(2), vec![],
            b"usage: zellij-agent-deck\nerror: unrecognized arguments: --attention-seq 1 --attachment-id example".to_vec(),
            context,
        );
        assert!(deck.notice.contains("outdated helper"));
        assert!(deck.notice.contains("reload Deck"));
    }

    #[test]
    fn selection_and_reply_target_survive_reordering() {
        let mut deck = AgentDeck {
            model: DeckModel {
                agents: vec![unread_agent("a", "work", 1), unread_agent("b", "work", 2)],
                selected: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        deck.set_input_mode(InputMode::Reply, "Reply");
        let mut changed = deck.model.agents[0].clone();
        changed.status = "needs_input".into();
        changed.revision = 2;
        deck.apply_agent_signal(changed);
        assert_eq!(deck.selected_agent().unwrap().key, "b");
        deck.cancel_input();
        assert_eq!(deck.selected_agent().unwrap().key, "b");
        let agents = vec![unread_agent("b", "work", 2), unread_agent("a", "work", 1)];
        deck.handle_result(
            Some(0),
            serde_json::to_vec(&agents).unwrap(),
            vec![],
            AgentDeck::context("list"),
        );
        assert_eq!(deck.selected_agent().unwrap().key, "b");
    }

    #[test]
    fn worktree_creation_previews_and_requires_confirmation() {
        let mut deck = AgentDeck {
            model: DeckModel {
                agents: vec![unread_agent("agent", "work", 7)],
                ..Default::default()
            },
            ..Default::default()
        };
        deck.browse_worktrees();
        let mut listed = AgentDeck::context("worktrees");
        listed.insert("dialog_id".into(), deck.dialog_id.to_string());
        deck.handle_result(Some(0), b"[]".to_vec(), vec![], listed);
        deck.handle_key(KeyWithModifier::new(BareKey::Char('n')));
        deck.input = "feature/search".into();
        deck.submit_input();
        assert_eq!(deck.mode, InputMode::WorktreeBranch);
        assert_eq!(deck.input, "feature/search");
        let mut planned = AgentDeck::context("worktree-plan");
        planned.insert("dialog_id".into(), deck.dialog_id.to_string());
        deck.handle_result(Some(0), br#"{"branch":"feature/search","path":"/tmp/example/search","base_head":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","base_branch":"main"}"#.to_vec(), vec![], planned);
        assert_eq!(deck.mode, InputMode::WorktreePrompt);
        deck.submit_input();
        assert_eq!(deck.mode, InputMode::ConfirmWorktree);
        assert!(!deck.worktree_busy);
        assert!(deck.worktree_prompt.is_empty());
        deck.handle_key(KeyWithModifier::new(BareKey::Char('y')));
        assert!(deck.worktree_busy);
    }

    #[test]
    fn cancelled_worktree_preview_cannot_reopen_the_dialog() {
        let mut deck = AgentDeck::default();
        let mut context = AgentDeck::context("worktree-plan");
        context.insert("dialog_id".into(), deck.dialog_id.to_string());
        deck.cancel_input();
        deck.handle_result(Some(0), b"{}".to_vec(), vec![], context);
        assert_eq!(deck.mode, InputMode::Browse);
        assert!(deck.worktree_plan.is_none());
    }

    #[test]
    fn next_attention_cycles_seen_requests_and_new_results_across_filters() {
        let mut pending = unread_agent("a", "work", 1);
        pending.status = "needs_input".into();
        pending.unread = false;
        let mut deck = AgentDeck {
            model: DeckModel {
                agents: vec![pending, unread_agent("b", "peer", 2)],
                filter: 6,
                query: "no matches".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        for key in ["a", "b", "a"] {
            deck.jump_next_attention();
            assert_eq!(deck.selected_agent().unwrap().key, key);
        }
    }

    #[test]
    fn focus_tracking_subscribes_to_client_and_navigation_updates() {
        for event in [
            EventType::ListClients,
            EventType::PaneUpdate,
            EventType::TabUpdate,
            EventType::SessionUpdate,
        ] {
            assert!(AgentDeck::subscribed_events().contains(&event));
        }
    }

    #[test]
    fn worktree_scope_matches_exact_checkout_and_can_be_cleared() {
        let mut model = DeckModel {
            worktree_scope: Some("/repo/search".into()),
            agents: vec![
                AgentRecord {
                    project_root: "/repo/search".into(),
                    ..unread_agent("a", "work", 1)
                },
                AgentRecord {
                    project_root: "/repo/search-next".into(),
                    ..unread_agent("b", "work", 2)
                },
            ],
            ..Default::default()
        };
        assert_eq!(model.matching_indices(None), vec![0]);
        model.clear_query();
        assert_eq!(model.matching_indices(None), vec![0, 1]);
    }

    #[test]
    fn searching_worktrees_and_opening_empty_checkout_requires_confirmation() {
        let mut deck = AgentDeck {
            worktrees: vec![
                WorktreeInfo {
                    path: "/repo/main".into(),
                    branch: "main".into(),
                    ..Default::default()
                },
                WorktreeInfo {
                    path: "/repo/search".into(),
                    branch: "feature/search".into(),
                    ..Default::default()
                },
            ],
            worktree_query: "search".into(),
            ..Default::default()
        };
        assert_eq!(deck.worktree_indices(), vec![1]);
        deck.open_selected_worktree();
        assert_eq!(deck.mode, InputMode::ConfirmWorktree);
        assert_eq!(deck.worktree_plan.as_ref().unwrap().path, "/repo/search");
        assert!(deck.worktree_plan.as_ref().unwrap().existing);
        assert!(!deck.worktree_busy);
    }

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
        assert!(truncate("سلام", 3).width() <= 3);
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
                        status: "working".into(),
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
    fn opencode_records_deserialize_and_are_resumable_after_detaching() {
        let mut agent: AgentRecord = serde_json::from_str(
            r#"{"kind":"opencode","opencode_session_id":"ses_example","pane_id":7,"zellij_session":"dev"}"#,
        ).unwrap();
        assert!(!is_resumable(&agent));
        agent.pane_id = None;
        assert!(is_resumable(&agent));
        let model = DeckModel {
            agents: vec![agent.clone()],
            filter: 6,
            ..Default::default()
        };
        assert!(model.matches_filter(&agent));
        agent.opencode_session_id.clear();
        assert!(!is_resumable(&agent));
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
                    status: "working".into(),
                    ..Default::default()
                },
                AgentRecord {
                    key: "codex:parent".into(),
                    zellij_session: "work".into(),
                    pane_id: Some(7),
                    status: "working".into(),
                    ..Default::default()
                },
                AgentRecord {
                    key: "subagent:parent:last".into(),
                    kind: "subagent".into(),
                    parent_key: "codex:parent".into(),
                    zellij_session: "work".into(),
                    pane_id: Some(7),
                    status: "working".into(),
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
    fn pane_close_detaches_only_the_matching_attachment_generation() {
        let agents = vec![
            AgentRecord {
                key: "codex:closed".into(),
                zellij_session: "work".into(),
                pane_id: Some(7),
                status: "working".into(),
                attachment_id: "generation-a".into(),
                ..Default::default()
            },
            AgentRecord {
                key: "codex:other-session".into(),
                zellij_session: "other".into(),
                pane_id: Some(7),
                status: "working".into(),
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
