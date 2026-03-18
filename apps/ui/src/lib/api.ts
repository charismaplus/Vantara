import { invoke } from "@tauri-apps/api/core";
import type {
  DeleteProjectResult,
  LayoutNode,
  LaunchProfile,
  PaneLaunchState,
  PastePayload,
  Project,
  StackItem,
  TerminalSession,
  WorkspaceSnapshot,
  WorkspaceTab,
} from "@workspace-terminal/contracts";

import { isTauriRuntime } from "./runtime";

type SplitDirection = "horizontal" | "vertical";
type PaneOwner = "user" | "ai";
type SplitZoneKind = "default" | "aiWorkspace";
type TabViewport = { width: number; height: number };

type CreateSessionArgs = {
  projectId: string;
  tabId: string;
  stackId: string;
  title?: string;
  program?: string;
  args?: string[] | null;
  cwd?: string;
  launchProfile?: LaunchProfile;
};

let mockCounter = 0;
const mockState = createMockState();

export async function listProjects() {
  if (!isTauriRuntime()) {
    return [...mockState.projects];
  }

  const projects = await invoke<unknown[]>("list_projects");
  return projects.map(normalizeProject);
}

export async function createProject(name: string, path: string) {
  if (!isTauriRuntime()) {
    const project: Project = {
      id: nextId("project"),
      name,
      path,
      color: mockColors[mockState.projects.length % mockColors.length],
      icon: null,
      lastOpenedAt: Date.now().toString(),
      createdAt: Date.now().toString(),
    };

    const defaultTab = createMockTab("main");
    mockState.projects.unshift(project);
    mockState.workspaces.set(project.id, {
      projectId: project.id,
      activeTabId: defaultTab.id,
      tabs: [defaultTab],
      sessions: [],
    });
    return project;
  }

  const project = await invoke<unknown>("create_project", { name, path });
  return normalizeProject(project);
}

export async function renameProject(projectId: string, name: string) {
  const nextName = name.trim();
  if (!nextName) {
    throw new Error("Project name is required");
  }

  if (!isTauriRuntime()) {
    const project = mockState.projects.find((entry) => entry.id === projectId);
    if (!project) {
      throw new Error("Project not found");
    }
    project.name = nextName;
    return { ...project };
  }

  const project = await invoke<unknown>("rename_project", { projectId, name: nextName });
  return normalizeProject(project);
}

export async function deleteProject(projectId: string) {
  if (!isTauriRuntime()) {
    const index = mockState.projects.findIndex((entry) => entry.id === projectId);
    if (index < 0) {
      throw new Error("Project not found");
    }

    const nextProjectId = mockState.projects[index + 1]?.id ?? mockState.projects[index - 1]?.id ?? null;
    mockState.projects.splice(index, 1);
    mockState.workspaces.delete(projectId);
    return {
      deletedProjectId: projectId,
      nextProjectId,
    } satisfies DeleteProjectResult;
  }

  const result = await invoke<unknown>("delete_project", { projectId });
  return normalizeDeleteProjectResult(result);
}

export async function openWorkspace(projectId: string) {
  if (!isTauriRuntime()) {
    const snapshot = getMockWorkspace(projectId);
    const project = mockState.projects.find((entry) => entry.id === projectId);
    if (project) {
      project.lastOpenedAt = Date.now().toString();
    }
    return cloneSnapshot(snapshot);
  }

  const snapshot = await invoke<unknown>("open_workspace", { projectId });
  return normalizeWorkspaceSnapshot(snapshot, projectId);
}

export async function createTab(projectId: string, title?: string) {
  if (!isTauriRuntime()) {
    const snapshot = getMockWorkspace(projectId);
    const tab = createMockTab(title ?? `tab-${snapshot.tabs.length + 1}`);
    snapshot.tabs.push(tab);
    snapshot.activeTabId = tab.id;
    return cloneSnapshot(snapshot);
  }

  const snapshot = await invoke<unknown>("create_tab", { projectId, title });
  return normalizeWorkspaceSnapshot(snapshot, projectId);
}

export async function closeTab(projectId: string, tabId: string) {
  if (!isTauriRuntime()) {
    const snapshot = getMockWorkspace(projectId);
    const index = snapshot.tabs.findIndex((tab) => tab.id === tabId);
    if (index < 0) {
      throw new Error("Tab not found");
    }

    const removed = snapshot.tabs.splice(index, 1)[0];
    const removedSessionIds = collectSessionIds(removed.root);
    snapshot.sessions = snapshot.sessions.filter((session) => !removedSessionIds.includes(session.id));

    if (!snapshot.tabs.length) {
      const tab = createMockTab("main");
      snapshot.tabs = [tab];
      snapshot.activeTabId = tab.id;
    } else if (snapshot.activeTabId === tabId) {
      snapshot.activeTabId = snapshot.tabs[Math.max(0, index - 1)]?.id ?? snapshot.tabs[0].id;
    }

    return cloneSnapshot(snapshot);
  }

  const snapshot = await invoke<unknown>("close_tab", { projectId, tabId });
  return normalizeWorkspaceSnapshot(snapshot, projectId);
}

export async function renameTab(projectId: string, tabId: string, title: string) {
  if (!isTauriRuntime()) {
    const snapshot = getMockWorkspace(projectId);
    const tab = snapshot.tabs.find((entry) => entry.id === tabId);
    if (!tab) {
      throw new Error("Tab not found");
    }
    tab.title = title;
    return cloneSnapshot(snapshot);
  }

  const snapshot = await invoke<unknown>("rename_tab", { projectId, tabId, title });
  return normalizeWorkspaceSnapshot(snapshot, projectId);
}

export async function setActiveTab(projectId: string, tabId: string) {
  if (!isTauriRuntime()) {
    const snapshot = getMockWorkspace(projectId);
    snapshot.activeTabId = tabId;
    return cloneSnapshot(snapshot);
  }

  const snapshot = await invoke<unknown>("set_active_tab", { projectId, tabId });
  return normalizeWorkspaceSnapshot(snapshot, projectId);
}

export async function splitPane(projectId: string, tabId: string, stackId: string, direction: SplitDirection) {
  if (!isTauriRuntime()) {
    const snapshot = getMockWorkspace(projectId);
    const tab = requireTab(snapshot, tabId);
    if (!splitMockStack(tab, stackId, direction)) {
      throw new Error("Target stack not found");
    }
    return cloneSnapshot(snapshot);
  }

  const snapshot = await invoke<unknown>("split_pane", { projectId, tabId, stackId, direction });
  return normalizeWorkspaceSnapshot(snapshot, projectId);
}

export async function createSession(args: CreateSessionArgs) {
  if (!isTauriRuntime()) {
    const snapshot = getMockWorkspace(args.projectId);
    const tab = requireTab(snapshot, args.tabId);
    const session: TerminalSession = {
      id: nextId("session"),
      projectId: args.projectId,
      title: args.title ?? args.program ?? "Terminal",
      program: args.program ?? "powershell",
      args: args.args ?? null,
      launchProfile: args.launchProfile ?? "terminal",
      tmuxShimEnabled: (args.launchProfile ?? "terminal") !== "terminal",
      cwd: args.cwd ?? mockState.projects.find((project) => project.id === args.projectId)?.path ?? "D:\\",
      status: "running",
      startedAt: Date.now().toString(),
      endedAt: null,
      exitCode: null,
    };

    attachSessionToMockStack(tab.root, args.stackId, session.id, session.title);
    tab.activePaneId = args.stackId;
    snapshot.sessions = [session, ...snapshot.sessions.filter((entry) => entry.id !== session.id)];
    return cloneSnapshot(snapshot);
  }

  console.debug("[launcher] create_session start", {
    projectId: args.projectId,
    tabId: args.tabId,
    stackId: args.stackId,
    program: args.program,
    args: args.args,
  });
  const snapshot = await invoke<unknown>("create_session", {
    projectId: args.projectId,
    tabId: args.tabId,
    stackId: args.stackId,
    title: args.title,
    program: args.program,
    args: args.args,
    cwd: args.cwd,
    launchProfile: args.launchProfile,
  });
  console.debug("[launcher] create_session end", {
    projectId: args.projectId,
    stackId: args.stackId,
  });
  return normalizeWorkspaceSnapshot(snapshot, args.projectId);
}

export async function setActiveStackItem(projectId: string, tabId: string, stackId: string, itemId: string) {
  if (!isTauriRuntime()) {
    const snapshot = getMockWorkspace(projectId);
    const tab = requireTab(snapshot, tabId);
    setMockActiveStackItem(tab.root, stackId, itemId);
    return cloneSnapshot(snapshot);
  }

  const snapshot = await invoke<unknown>("set_active_stack_item_command", { projectId, tabId, stackId, itemId });
  return normalizeWorkspaceSnapshot(snapshot, projectId);
}

export async function closePane(projectId: string, tabId: string, stackId: string) {
  if (!isTauriRuntime()) {
    const snapshot = getMockWorkspace(projectId);
    const tab = requireTab(snapshot, tabId);
    const sessionIds = closeMockPane(tab.root, stackId);
    if (!sessionIds) {
      throw new Error("Pane not found");
    }
    if (!tab.activePaneId || tab.activePaneId === stackId) {
      tab.activePaneId = getFirstStackId(tab.root);
    }
    snapshot.sessions = snapshot.sessions.filter((session) => !sessionIds.includes(session.id));
    return cloneSnapshot(snapshot);
  }

  const snapshot = await invoke<unknown>("close_pane", { projectId, tabId, stackId });
  return normalizeWorkspaceSnapshot(snapshot, projectId);
}

export async function writeSessionInput(sessionId: string, input: string) {
  if (!isTauriRuntime()) {
    void sessionId;
    void input;
    return;
  }

  return invoke("write_session_input", { sessionId, input });
}

export async function resizeSession(sessionId: string, cols: number, rows: number) {
  if (!isTauriRuntime()) {
    void sessionId;
    void cols;
    void rows;
    return;
  }

  return invoke("resize_session", { sessionId, cols, rows });
}

export async function reportTabViewport(projectId: string, tabId: string, width: number, height: number) {
  if (!isTauriRuntime()) {
    mockState.tabViewports.set(tabViewportKey(projectId, tabId), { width, height });
    return;
  }

  return invoke("report_tab_viewport", { projectId, tabId, width, height });
}

export async function readClipboardPayload(): Promise<PastePayload> {
  if (!isTauriRuntime()) {
    try {
      const text = await navigator.clipboard.readText();
      return text ? { kind: "text", text } : { kind: "empty" };
    } catch {
      return { kind: "empty" };
    }
  }

  const payload = await invoke<unknown>("read_clipboard_payload");
  return normalizePastePayload(payload);
}

function createMockState() {
  const projectA: Project = {
    id: "project-app",
    name: "Workspace Terminal",
    path: "D:\\FutureTeam\\00002.VantaraC",
    color: "#0ea5e9",
    icon: null,
    lastOpenedAt: Date.now().toString(),
    createdAt: Date.now().toString(),
  };
  const projectB: Project = {
    id: "project-api",
    name: "Backend Services",
    path: "D:\\FutureTeam\\backend-services",
    color: "#34d399",
    icon: null,
    lastOpenedAt: (Date.now() - 1000).toString(),
    createdAt: (Date.now() - 1000).toString(),
  };

  const appTab = createMockTab("main");

  const backendTab = createMockTab("api");
  splitMockStack(backendTab, getFirstStackId(backendTab.root)!, "vertical", "user");
  let backendStackIds = collectStackIds(backendTab.root);
  splitMockStack(backendTab, backendStackIds[1], "horizontal", "ai");
  backendStackIds = collectStackIds(backendTab.root);
  const backendSessions: TerminalSession[] = [
    {
      id: "session-api-1",
      projectId: projectB.id,
      title: "server",
      program: "powershell",
      args: null,
      launchProfile: "terminal",
      tmuxShimEnabled: false,
      cwd: projectB.path,
      status: "running",
      startedAt: Date.now().toString(),
      endedAt: null,
      exitCode: null,
    },
    {
      id: "session-api-2",
      projectId: projectB.id,
      title: "tests",
      program: "powershell",
      args: null,
      launchProfile: "terminal",
      tmuxShimEnabled: false,
      cwd: projectB.path,
      status: "running",
      startedAt: Date.now().toString(),
      endedAt: null,
      exitCode: null,
    },
    {
      id: "session-api-3",
      projectId: projectB.id,
      title: "review-agent",
      program: "codex",
      args: ["--full-auto"],
      launchProfile: "codexFullAuto",
      tmuxShimEnabled: true,
      cwd: projectB.path,
      status: "running",
      startedAt: Date.now().toString(),
      endedAt: null,
      exitCode: null,
    },
  ];
  attachSessionToMockStack(backendTab.root, backendStackIds[0], backendSessions[0].id, backendSessions[0].title);
  attachSessionToMockStack(backendTab.root, backendStackIds[1], backendSessions[1].id, backendSessions[1].title);
  attachSessionToMockStack(backendTab.root, backendStackIds[2], backendSessions[2].id, backendSessions[2].title);

  return {
    projects: [projectA, projectB],
    workspaces: new Map<string, WorkspaceSnapshot>([
      [
        projectA.id,
        {
          projectId: projectA.id,
          activeTabId: appTab.id,
          tabs: [appTab],
          sessions: [],
        },
      ],
      [
        projectB.id,
        {
          projectId: projectB.id,
          activeTabId: backendTab.id,
          tabs: [backendTab],
          sessions: backendSessions,
        },
      ],
    ]),
    tabViewports: new Map<string, TabViewport>(),
    counter: 1,
  };
}

function cloneSnapshot(snapshot: WorkspaceSnapshot): WorkspaceSnapshot {
  return JSON.parse(JSON.stringify(snapshot)) as WorkspaceSnapshot;
}

function getMockWorkspace(projectId: string) {
  const snapshot = mockState.workspaces.get(projectId);
  if (!snapshot) {
    throw new Error("Workspace not found");
  }
  return snapshot;
}

function createMockTab(title: string): WorkspaceTab {
  const root = createEmptyStack(1, "user", null);

  return {
    id: nextId("tab"),
    title,
    root,
    nextPaneOrdinal: 2,
    activePaneId: root.id,
  };
}

function createEmptyStack(
  paneOrdinal: number,
  createdBy: "user" | "ai" = "user",
  sourcePaneId: string | null = null,
  launchState?: PaneLaunchState,
): LayoutNode {
  const itemId = nextId("item");
  return {
    type: "stack",
    id: nextId("stack"),
    paneOrdinal,
    paneLabel: `P${paneOrdinal}`,
    createdBy,
    launchState: launchState ?? (createdBy === "ai" ? "launched" : "unlaunched"),
    sourcePaneId,
    activeItemId: itemId,
    items: [
      {
        id: itemId,
        kind: "terminal",
        sessionId: null,
        title: "Empty",
      },
    ],
  };
}

function nextId(prefix: string) {
  mockCounter += 1;
  return `${prefix}-${mockCounter}`;
}

function normalizeProject(value: unknown): Project {
  const record = asRecord(value);
  return {
    id: asString(record.id, nextId("project")),
    name: asString(record.name, "Project"),
    path: asString(record.path, "D:\\"),
    color: asString(record.color, mockColors[0]),
    icon: typeof record.icon === "string" ? record.icon : null,
    lastOpenedAt: typeof record.lastOpenedAt === "string"
      ? record.lastOpenedAt
      : typeof record.last_opened_at === "string"
        ? record.last_opened_at
        : null,
    createdAt: typeof record.createdAt === "string"
      ? record.createdAt
      : typeof record.created_at === "string"
        ? record.created_at
        : Date.now().toString(),
  };
}

function normalizeDeleteProjectResult(value: unknown): DeleteProjectResult {
  const record = asRecord(value);
  return {
    deletedProjectId: asString(record.deletedProjectId ?? record.deleted_project_id, ""),
    nextProjectId: normalizeNullableString(record.nextProjectId ?? record.next_project_id),
  };
}

function normalizeWorkspaceSnapshot(value: unknown, fallbackProjectId?: string): WorkspaceSnapshot {
  const record = asRecord(value);
  const projectId = asString(record.projectId ?? record.project_id, fallbackProjectId ?? nextId("project"));
  const sessions = Array.isArray(record.sessions) ? record.sessions.map((entry) => normalizeSession(entry, projectId)) : [];
  const tabsSource = Array.isArray(record.tabs) ? record.tabs : [];
  const tabs = tabsSource.length > 0
    ? tabsSource.map((entry, index) => normalizeTab(entry, index + 1))
    : [createMockTab("main")];
  const activeTabId = asString(record.activeTabId ?? record.active_tab_id, tabs[0]?.id ?? null);

  return {
    projectId,
    activeTabId: tabs.some((tab) => tab.id === activeTabId) ? activeTabId : tabs[0]?.id ?? null,
    tabs,
    sessions,
  };
}

function normalizeTab(value: unknown, fallbackOrdinal: number) {
  const record = asRecord(value);
  const title = asString(record.title, fallbackOrdinal === 1 ? "main" : `tab-${fallbackOrdinal}`);
  const fallback = createMockTab(title);
  const root = normalizeLayoutNode(record.root, 1, "user", "unlaunched", null);

  return {
    id: asString(record.id, fallback.id),
    title,
    root,
    nextPaneOrdinal: asNumber(record.nextPaneOrdinal ?? record.next_pane_ordinal, Math.max(countStacks(root) + 1, 2)),
    activePaneId: asString(record.activePaneId ?? record.active_pane_id, getFirstStackId(root)),
  };
}

function normalizeLayoutNode(
  value: unknown,
  fallbackOrdinal: number,
  fallbackOwner: PaneOwner,
  fallbackLaunchState: PaneLaunchState,
  fallbackSourcePaneId: string | null,
): LayoutNode {
  const record = asRecord(value);
  const type = asString(record.type, "stack");
  if (type === "split") {
    const direction = asString(record.direction, "horizontal") === "vertical" ? "vertical" : "horizontal";
    const zoneKind = normalizeSplitZoneKind(record.zoneKind ?? record.zone_kind);
    const childValues = Array.isArray(record.children) ? record.children : [];
    const children = childValues.length > 0
      ? childValues.map((child, index) => normalizeLayoutNode(child, fallbackOrdinal + index, fallbackOwner, fallbackLaunchState, fallbackSourcePaneId))
      : [createEmptyStack(fallbackOrdinal, fallbackOwner, fallbackSourcePaneId, fallbackLaunchState)];
    const defaultSizes = children.length === 2
      ? defaultSplitSizes(direction)
      : children.map((_child, index) => (index === 0 ? 100 - ((children.length - 1) * Math.floor(100 / children.length)) : Math.floor(100 / children.length)));
    const sizes = Array.isArray(record.sizes) && record.sizes.length === children.length
      ? record.sizes.map((entry, index) => asNumber(entry, defaultSizes[index] ?? 50))
      : defaultSizes;

    return {
      type: "split",
      id: asString(record.id, nextId("split")),
      direction,
      zoneKind,
      sizes,
      children,
    };
  }

  const createdBy = normalizePaneOwner(record.createdBy ?? record.created_by, fallbackOwner);
  const paneOrdinal = asNumber(record.paneOrdinal ?? record.pane_ordinal, fallbackOrdinal);
  const paneLabel = asString(record.paneLabel ?? record.pane_label, `P${paneOrdinal}`);
  const launchState = normalizeLaunchState(record.launchState ?? record.launch_state, createdBy === "ai" ? "launched" : fallbackLaunchState);
  const itemsSource = Array.isArray(record.items) ? record.items : [];
  const items = itemsSource.length > 0 ? itemsSource.map(normalizeStackItem) : [emptyStackItem()];
  const activeItemId = asString(record.activeItemId ?? record.active_item_id, items[0]?.id ?? nextId("item"));

  return {
    type: "stack",
    id: asString(record.id, nextId("stack")),
    paneOrdinal,
    paneLabel,
    createdBy,
    launchState,
    sourcePaneId: typeof record.sourcePaneId === "string"
      ? record.sourcePaneId
      : typeof record.source_pane_id === "string"
        ? record.source_pane_id
        : fallbackSourcePaneId,
    activeItemId: items.some((item) => item.id === activeItemId) ? activeItemId : items[0].id,
    items,
  };
}

function normalizeStackItem(value: unknown): StackItem {
  const record = asRecord(value);
  return {
    id: asString(record.id, nextId("item")),
    kind: "terminal",
    sessionId: typeof record.sessionId === "string"
      ? record.sessionId
      : typeof record.session_id === "string"
        ? record.session_id
        : null,
    title: asString(record.title, "Terminal"),
  };
}

function normalizeSession(value: unknown, fallbackProjectId: string): TerminalSession {
  const record = asRecord(value);
  return {
    id: asString(record.id, nextId("session")),
    projectId: asString(record.projectId ?? record.project_id, fallbackProjectId),
    title: asString(record.title, "Terminal"),
    program: asString(record.program ?? record.shell, "powershell"),
    args: normalizeArgs(record.args ?? record.args_json),
    launchProfile: normalizeLaunchProfile(record.launchProfile ?? record.launch_profile),
    tmuxShimEnabled: Boolean(record.tmuxShimEnabled ?? record.tmux_shim_enabled),
    cwd: asString(record.cwd, "D:\\"),
    status: normalizeSessionStatus(record.status),
    startedAt: normalizeNullableString(record.startedAt ?? record.started_at),
    endedAt: normalizeNullableString(record.endedAt ?? record.ended_at),
    exitCode: typeof record.exitCode === "number"
      ? record.exitCode
      : typeof record.exit_code === "number"
        ? record.exit_code
        : null,
  };
}

function normalizeSessionStatus(value: unknown): TerminalSession["status"] {
  return value === "starting" || value === "running" || value === "exited" || value === "failed" ? value : "failed";
}

function normalizeLaunchProfile(value: unknown): LaunchProfile {
  return value === "claude"
    || value === "claudeUnsafe"
    || value === "codex"
    || value === "codexFullAuto"
    || value === "terminal"
    ? value
    : "terminal";
}

function normalizeArgs(value: unknown): string[] | null {
  if (Array.isArray(value)) {
    return value.filter((entry): entry is string => typeof entry === "string");
  }
  if (typeof value === "string") {
    try {
      const parsed = JSON.parse(value);
      return Array.isArray(parsed) ? parsed.filter((entry): entry is string => typeof entry === "string") : null;
    } catch {
      return null;
    }
  }
  return null;
}

function normalizePaneOwner(value: unknown, fallback: PaneOwner): PaneOwner {
  return value === "ai" || value === "user" ? value : fallback;
}

function normalizeLaunchState(value: unknown, fallback: PaneLaunchState): PaneLaunchState {
  return value === "launched" || value === "unlaunched" ? value : fallback;
}

function normalizeSplitZoneKind(value: unknown): SplitZoneKind {
  return value === "aiWorkspace" ? "aiWorkspace" : "default";
}

function normalizeNullableString(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function normalizePastePayload(value: unknown): PastePayload {
  const record = asRecord(value);
  if (record.kind === "files" && Array.isArray(record.paths)) {
    return {
      kind: "files",
      paths: record.paths.filter((entry): entry is string => typeof entry === "string"),
    };
  }

  if (record.kind === "imagePath" && typeof record.imagePath === "string") {
    return {
      kind: "imagePath",
      imagePath: record.imagePath,
    };
  }

  if (record.kind === "text" && typeof record.text === "string") {
    return {
      kind: "text",
      text: record.text,
    };
  }

  return { kind: "empty" };
}

function asRecord(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" ? (value as Record<string, unknown>) : {};
}

function asString(value: unknown, fallback: string | null): string {
  if (typeof value === "string" && value.trim().length > 0) {
    return value;
  }
  return fallback ?? "";
}

function asNumber(value: unknown, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function emptyStackItem(): StackItem {
  return {
    id: nextId("item"),
    kind: "terminal",
    sessionId: null,
    title: "Empty",
  };
}

function countStacks(node: LayoutNode): number {
  if (node.type === "stack") {
    return 1;
  }
  return node.children.reduce((sum, child) => sum + countStacks(child), 0);
}

function requireTab(snapshot: WorkspaceSnapshot, tabId: string) {
  const tab = snapshot.tabs.find((entry) => entry.id === tabId);
  if (!tab) {
    throw new Error("Tab not found");
  }
  return tab;
}

function splitMockStack(
  tab: WorkspaceTab,
  targetId: string,
  direction: SplitDirection,
  createdBy: PaneOwner = "user",
): boolean {
  return splitMockNode(tab.root, tab, targetId, direction, createdBy);
}

function splitMockNode(
  node: LayoutNode,
  tab: WorkspaceTab,
  targetId: string,
  direction: SplitDirection,
  createdBy: PaneOwner,
): boolean {
  if (node.type === "stack" && node.id === targetId) {
    const current = JSON.parse(JSON.stringify(node)) as LayoutNode;
    const replacement: LayoutNode = {
      type: "split",
      id: nextId("split"),
      direction,
      zoneKind: "default",
      sizes: defaultSplitSizes(direction),
      children: [current, createEmptyStack(tab.nextPaneOrdinal, createdBy, node.id)],
    };
    tab.nextPaneOrdinal += 1;
    replaceNode(node, replacement);
    return true;
  }

  if (node.type === "split") {
    return node.children.some((child) => splitMockNode(child, tab, targetId, direction, createdBy));
  }

  return false;
}

function attachSessionToMockStack(node: LayoutNode, targetId: string, sessionId: string, title: string): boolean {
  if (node.type === "stack" && node.id === targetId) {
    const first = node.items[0];
    if (node.items.length <= 1) {
      node.items = [
        {
          id: first?.id ?? nextId("item"),
          kind: "terminal",
          sessionId,
          title,
        },
      ];
      node.launchState = "launched";
      node.activeItemId = node.items[0].id;
      return true;
    }

    const item = {
      id: nextId("item"),
      kind: "terminal" as const,
      sessionId,
      title,
    };
    node.items.push(item);
    node.launchState = "launched";
    node.activeItemId = item.id;
    return true;
  }

  if (node.type === "split") {
    return node.children.some((child) => attachSessionToMockStack(child, targetId, sessionId, title));
  }

  return false;
}

function setMockActiveStackItem(node: LayoutNode, targetId: string, itemId: string): boolean {
  if (node.type === "stack" && node.id === targetId && node.items.some((item) => item.id === itemId)) {
    node.activeItemId = itemId;
    return true;
  }

  if (node.type === "split") {
    return node.children.some((child) => setMockActiveStackItem(child, targetId, itemId));
  }

  return false;
}

function closeMockPane(node: LayoutNode, targetId: string): string[] | null {
  if (node.type === "stack" && node.id === targetId) {
    const sessionIds = collectSessionIds(node);
    clearMockStack(node);
    return sessionIds;
  }

  if (node.type !== "split") {
    return null;
  }

  for (let index = 0; index < node.children.length; index += 1) {
    const child = node.children[index];

    if (child.type === "stack" && child.id === targetId) {
      const sessionIds = collectSessionIds(child);
      node.children.splice(index, 1);
      if (node.children.length === 1) {
        replaceNode(node, node.children[0]);
      } else {
        node.sizes = defaultSizesForCount(node.direction, node.children.length);
      }
      return sessionIds;
    }

    const nested = closeMockPane(child, targetId);
    if (nested) {
      if (node.type === "split" && node.children.length === 1) {
        replaceNode(node, node.children[0]);
      } else if (node.type === "split") {
        node.sizes = defaultSizesForCount(node.direction, node.children.length);
      }
      return nested;
    }
  }

  return null;
}

function clearMockStack(node: Extract<LayoutNode, { type: "stack" }>) {
  const itemId = nextId("item");
  node.activeItemId = itemId;
  node.launchState = node.createdBy === "user" ? "unlaunched" : "launched";
  node.items = [
    {
      id: itemId,
      kind: "terminal",
      sessionId: null,
      title: "Empty",
    },
  ];
}

function collectSessionIds(node: LayoutNode): string[] {
  if (node.type === "stack") {
    return node.items.flatMap((item) => (item.sessionId ? [item.sessionId] : []));
  }

  return node.children.flatMap((child) => collectSessionIds(child));
}

function replaceNode(target: LayoutNode, replacement: LayoutNode) {
  for (const key of Object.keys(target)) {
    delete (target as Record<string, unknown>)[key];
  }
  Object.assign(target, replacement);
}

function defaultSizesForCount(direction: SplitDirection, childCount: number): number[] {
  if (childCount <= 0) {
    return [];
  }

  if (childCount === 1) {
    return [100];
  }

  if (childCount === 2) {
    return defaultSplitSizes(direction);
  }

  const base = Math.floor(100 / childCount);
  const remainder = 100 - base * childCount;
  return Array.from({ length: childCount }, (_, index) => (index === 0 ? base + remainder : base));
}

function getFirstStackId(node: LayoutNode): string | null {
  if (node.type === "stack") {
    return node.id;
  }

  for (const child of node.children) {
    const id = getFirstStackId(child);
    if (id) {
      return id;
    }
  }

  return null;
}

function collectStackIds(node: LayoutNode): string[] {
  if (node.type === "stack") {
    return [node.id];
  }

  return node.children.flatMap((child) => collectStackIds(child));
}

function defaultSplitSizes(direction: SplitDirection): [number, number] {
  void direction;
  return [50, 50];
}

function tabViewportKey(projectId: string, tabId: string) {
  return `${projectId}:${tabId}`;
}

const mockColors = ["#0ea5e9", "#34d399", "#f97316", "#facc15"];
