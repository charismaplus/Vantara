import { invoke } from "@tauri-apps/api/core";
import type {
  DeleteProjectResult,
  LayoutNode,
  LaunchProfile,
  PaneLaunchState,
  PastePayload,
  Project,
  ProjectWorkspaceSnapshot,
  StackItem,
  TerminalSession,
  WorkspaceSession,
  WorkspaceSessionCreatedBy,
  WorkspaceSnapshot,
  WorkspaceTab,
} from "../../../../packages/contracts/src/index.ts";

import { isTauriRuntime } from "./runtime";

type SplitDirection = "horizontal" | "vertical";

type CreateWorkspaceSessionArgs = {
  name?: string;
  createdBy?: WorkspaceSessionCreatedBy;
  sourceSessionId?: string | null;
};

type CreateSessionArgs = {
  projectId: string;
  workspaceSessionId: string;
  windowId?: string | null;
  stackId?: string | null;
  title?: string;
  program?: string;
  args?: string[] | null;
  cwd?: string;
  launchProfile?: LaunchProfile;
};

type MockState = {
  counter: number;
  projects: Project[];
  projectSnapshots: Map<string, ProjectWorkspaceSnapshot>;
  sessionSnapshots: Map<string, WorkspaceSnapshot>;
};

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
      color: "#0ea5e9",
      icon: null,
      lastOpenedAt: now(),
      createdAt: now(),
    };
    const session = createMockWorkspaceSession(project.id, "main", "user", null);
    mockState.projects.unshift(project);
    mockState.projectSnapshots.set(project.id, { projectId: project.id, sessions: [session] });
    mockState.sessionSnapshots.set(session.id, createEmptySessionSnapshot(project.id, session.id));
    return project;
  }

  return normalizeProject(await invoke("create_project", { name, path }));
}

export async function renameProject(projectId: string, name: string) {
  const nextName = name.trim();
  if (!nextName) {
    throw new Error("Project name is required");
  }

  if (!isTauriRuntime()) {
    const project = requireProject(projectId);
    project.name = nextName;
    return { ...project };
  }

  return normalizeProject(await invoke("rename_project", { projectId, name: nextName }));
}

export async function deleteProject(projectId: string) {
  if (!isTauriRuntime()) {
    const index = mockState.projects.findIndex((entry) => entry.id === projectId);
    if (index < 0) {
      throw new Error("Project not found");
    }
    const nextProjectId = mockState.projects[index + 1]?.id ?? mockState.projects[index - 1]?.id ?? null;
    const projectSnapshot = mockState.projectSnapshots.get(projectId);
    for (const session of projectSnapshot?.sessions ?? []) {
      mockState.sessionSnapshots.delete(session.id);
    }
    mockState.projectSnapshots.delete(projectId);
    mockState.projects.splice(index, 1);
    return { deletedProjectId: projectId, nextProjectId } satisfies DeleteProjectResult;
  }

  return normalizeDeleteProjectResult(await invoke("delete_project", { projectId }));
}

export async function openProject(projectId: string) {
  if (!isTauriRuntime()) {
    return cloneProjectSnapshot(getMockProjectSnapshot(projectId));
  }

  return normalizeProjectWorkspaceSnapshot(await invoke("open_project", { projectId }), projectId);
}

export async function openSession(projectId: string, workspaceSessionId: string) {
  if (!isTauriRuntime()) {
    touchMockSession(projectId, workspaceSessionId);
    return cloneSessionSnapshot(getMockSessionSnapshot(projectId, workspaceSessionId));
  }

  return normalizeSessionWorkspaceSnapshot(
    await invoke("open_session", { projectId, workspaceSessionId }),
    projectId,
    workspaceSessionId,
  );
}

export async function createWorkspaceSession(projectId: string, args: CreateWorkspaceSessionArgs = {}) {
  if (!isTauriRuntime()) {
    const snapshot = getMockProjectSnapshot(projectId);
    const session = createMockWorkspaceSession(
      projectId,
      args.name ?? `session-${snapshot.sessions.length + 1}`,
      args.createdBy ?? "user",
      args.sourceSessionId ?? null,
    );
    snapshot.sessions.unshift(session);
    mockState.sessionSnapshots.set(session.id, createEmptySessionSnapshot(projectId, session.id));
    return { ...session };
  }

  return normalizeWorkspaceSession(
    await invoke("create_workspace_session", {
      projectId,
      name: args.name,
      createdBy: args.createdBy,
      sourceSessionId: args.sourceSessionId,
    }),
    projectId,
  );
}

export async function renameWorkspaceSession(projectId: string, workspaceSessionId: string, name: string) {
  const nextName = name.trim();
  if (!nextName) {
    throw new Error("Session name is required");
  }

  if (!isTauriRuntime()) {
    const session = requireWorkspaceSession(projectId, workspaceSessionId);
    session.name = nextName;
    return { ...session };
  }

  return normalizeWorkspaceSession(
    await invoke("rename_workspace_session", { projectId, workspaceSessionId, name: nextName }),
    projectId,
  );
}

export async function deleteWorkspaceSession(projectId: string, workspaceSessionId: string) {
  if (!isTauriRuntime()) {
    const projectSnapshot = getMockProjectSnapshot(projectId);
    projectSnapshot.sessions = projectSnapshot.sessions.filter((entry) => entry.id !== workspaceSessionId);
    mockState.sessionSnapshots.delete(workspaceSessionId);
    if (!projectSnapshot.sessions.length) {
      const fallback = createMockWorkspaceSession(projectId, "main", "user", null);
      projectSnapshot.sessions = [fallback];
      mockState.sessionSnapshots.set(fallback.id, createEmptySessionSnapshot(projectId, fallback.id));
    }
    return cloneProjectSnapshot(projectSnapshot);
  }

  return normalizeProjectWorkspaceSnapshot(
    await invoke("delete_workspace_session", { projectId, workspaceSessionId }),
    projectId,
  );
}

export async function createWindow(projectId: string, workspaceSessionId: string, title?: string) {
  if (!isTauriRuntime()) {
    const snapshot = getMockSessionSnapshot(projectId, workspaceSessionId);
    const tab = createMockTab(title ?? `window-${snapshot.windows.length + 1}`);
    snapshot.windows.push(tab);
    snapshot.activeWindowId = tab.id;
    return cloneSessionSnapshot(snapshot);
  }

  return normalizeSessionWorkspaceSnapshot(
    await invoke("create_window", { projectId, workspaceSessionId, title }),
    projectId,
    workspaceSessionId,
  );
}

export async function closeWindow(projectId: string, workspaceSessionId: string, windowId: string) {
  if (!isTauriRuntime()) {
    const snapshot = getMockSessionSnapshot(projectId, workspaceSessionId);
    const index = snapshot.windows.findIndex((entry) => entry.id === windowId);
    if (index < 0) {
      throw new Error("Window not found");
    }
    const removed = snapshot.windows.splice(index, 1)[0];
    const removedSessionIds = collectSessionIds(removed.root);
    snapshot.terminals = snapshot.terminals.filter((entry) => !removedSessionIds.includes(entry.id));
    snapshot.activeWindowId = snapshot.windows[Math.max(0, index - 1)]?.id ?? snapshot.windows[0]?.id ?? null;
    return cloneSessionSnapshot(snapshot);
  }

  return normalizeSessionWorkspaceSnapshot(
    await invoke("close_window", { projectId, workspaceSessionId, windowId }),
    projectId,
    workspaceSessionId,
  );
}

export async function renameWindow(projectId: string, workspaceSessionId: string, windowId: string, title: string) {
  if (!isTauriRuntime()) {
    requireWindow(getMockSessionSnapshot(projectId, workspaceSessionId), windowId).title = title;
    return cloneSessionSnapshot(getMockSessionSnapshot(projectId, workspaceSessionId));
  }

  return normalizeSessionWorkspaceSnapshot(
    await invoke("rename_window", { projectId, workspaceSessionId, windowId, title }),
    projectId,
    workspaceSessionId,
  );
}

export async function setActiveWindow(projectId: string, workspaceSessionId: string, windowId: string) {
  if (!isTauriRuntime()) {
    const snapshot = getMockSessionSnapshot(projectId, workspaceSessionId);
    snapshot.activeWindowId = windowId;
    return cloneSessionSnapshot(snapshot);
  }

  return normalizeSessionWorkspaceSnapshot(
    await invoke("set_active_window", { projectId, workspaceSessionId, windowId }),
    projectId,
    workspaceSessionId,
  );
}

export async function splitPane(
  projectId: string,
  workspaceSessionId: string,
  windowId: string,
  stackId: string,
  direction: SplitDirection,
) {
  if (!isTauriRuntime()) {
    const snapshot = getMockSessionSnapshot(projectId, workspaceSessionId);
    const window = requireWindow(snapshot, windowId);
    if (!splitMockStack(window, stackId, direction)) {
      throw new Error("Target pane not found");
    }
    return cloneSessionSnapshot(snapshot);
  }

  return normalizeSessionWorkspaceSnapshot(
    await invoke("split_pane", { projectId, workspaceSessionId, tabId: windowId, stackId, direction }),
    projectId,
    workspaceSessionId,
  );
}

export async function createSession(args: CreateSessionArgs) {
  if (!isTauriRuntime()) {
    const snapshot = getMockSessionSnapshot(args.projectId, args.workspaceSessionId);
    const window = args.windowId ? requireWindow(snapshot, args.windowId) : ensureMockWindow(snapshot);
    const stackId = args.stackId ?? window.activePaneId ?? getFirstStackId(window.root);
    if (!stackId) {
      throw new Error("Target pane not found");
    }

    const session: TerminalSession = {
      id: nextId("terminal-session"),
      projectId: args.projectId,
      workspaceSessionId: args.workspaceSessionId,
      windowId: window.id,
      title: args.title ?? args.program ?? "Terminal",
      program: args.program ?? "powershell",
      args: args.args ?? null,
      launchProfile: args.launchProfile ?? "terminal",
      tmuxShimEnabled: (args.launchProfile ?? "terminal") !== "terminal",
      cwd: args.cwd ?? requireProject(args.projectId).path,
      status: "running",
      startedAt: now(),
      endedAt: null,
      exitCode: null,
    };

    attachSessionToStack(window.root, stackId, session.id, session.title);
    window.activePaneId = stackId;
    snapshot.activeWindowId = window.id;
    snapshot.terminals = [session, ...snapshot.terminals.filter((entry) => entry.id !== session.id)];
    return cloneSessionSnapshot(snapshot);
  }

  return normalizeSessionWorkspaceSnapshot(
    await invoke("create_session", {
      projectId: args.projectId,
      workspaceSessionId: args.workspaceSessionId,
      windowId: args.windowId,
      stackId: args.stackId,
      title: args.title,
      program: args.program,
      args: args.args,
      cwd: args.cwd,
      launchProfile: args.launchProfile,
    }),
    args.projectId,
    args.workspaceSessionId,
  );
}

export async function setActiveStackItem(
  projectId: string,
  workspaceSessionId: string,
  windowId: string,
  stackId: string,
  itemId: string,
) {
  if (!isTauriRuntime()) {
    const window = requireWindow(getMockSessionSnapshot(projectId, workspaceSessionId), windowId);
    setMockActiveStackItem(window.root, stackId, itemId);
    return cloneSessionSnapshot(getMockSessionSnapshot(projectId, workspaceSessionId));
  }

  return normalizeSessionWorkspaceSnapshot(
    await invoke("set_active_stack_item_command", { projectId, workspaceSessionId, tabId: windowId, stackId, itemId }),
    projectId,
    workspaceSessionId,
  );
}

export async function closePane(projectId: string, workspaceSessionId: string, windowId: string, stackId: string) {
  if (!isTauriRuntime()) {
    const snapshot = getMockSessionSnapshot(projectId, workspaceSessionId);
    const window = requireWindow(snapshot, windowId);
    const sessionIds = closeMockPane(window.root, stackId);
    if (!sessionIds) {
      throw new Error("Pane not found");
    }
    window.activePaneId = getFirstStackId(window.root);
    snapshot.terminals = snapshot.terminals.filter((entry) => !sessionIds.includes(entry.id));
    return cloneSessionSnapshot(snapshot);
  }

  return normalizeSessionWorkspaceSnapshot(
    await invoke("close_pane", { projectId, workspaceSessionId, tabId: windowId, stackId }),
    projectId,
    workspaceSessionId,
  );
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
    void projectId;
    void tabId;
    void width;
    void height;
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

  return normalizePastePayload(await invoke("read_clipboard_payload"));
}

function createMockState(): MockState {
  const state: MockState = {
    counter: 0,
    projects: [],
    projectSnapshots: new Map(),
    sessionSnapshots: new Map(),
  };
  const project: Project = {
    id: "project-demo",
    name: "Vantara",
    path: "D:\\FutureTeam\\00002.VantaraC",
    color: "#0ea5e9",
    icon: null,
    lastOpenedAt: now(),
    createdAt: now(),
  };
  const session: WorkspaceSession = {
    id: nextStateId(state, "workspace-session"),
    projectId: project.id,
    name: "main",
    createdBy: "user",
    sourceSessionId: null,
    lastOpenedAt: now(),
    createdAt: now(),
  };
  state.projects = [project];
  state.projectSnapshots.set(project.id, { projectId: project.id, sessions: [session] });
  state.sessionSnapshots.set(session.id, createEmptySessionSnapshot(project.id, session.id));
  return state;
}

function now() {
  return Date.now().toString();
}

function nextId(prefix: string) {
  return nextStateId(mockState, prefix);
}

function nextStateId(state: MockState, prefix: string) {
  state.counter += 1;
  return `${prefix}-${state.counter}`;
}

function cloneProjectSnapshot(snapshot: ProjectWorkspaceSnapshot) {
  return JSON.parse(JSON.stringify(snapshot)) as ProjectWorkspaceSnapshot;
}

function cloneSessionSnapshot(snapshot: WorkspaceSnapshot) {
  return JSON.parse(JSON.stringify(snapshot)) as WorkspaceSnapshot;
}

function createMockWorkspaceSession(
  projectId: string,
  name: string,
  createdBy: WorkspaceSessionCreatedBy,
  sourceSessionId: string | null,
): WorkspaceSession {
  return {
    id: nextId("workspace-session"),
    projectId,
    name,
    createdBy,
    sourceSessionId,
    lastOpenedAt: now(),
    createdAt: now(),
  };
}

function createEmptySessionSnapshot(projectId: string, workspaceSessionId: string): WorkspaceSnapshot {
  return {
    projectId,
    sessionId: workspaceSessionId,
    activeWindowId: null,
    windows: [],
    terminals: [],
  };
}

function createMockTab(title: string): WorkspaceTab {
  const root = createEmptyStack(1, "user", null);
  return {
    id: nextId("window"),
    title,
    root,
    nextPaneOrdinal: 2,
    activePaneId: root.id,
  };
}

function createEmptyStack(
  paneOrdinal: number,
  createdBy: "user" | "ai",
  sourcePaneId: string | null,
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
    items: [{ id: itemId, kind: "terminal", sessionId: null, title: "Empty" }],
  };
}

function requireProject(projectId: string) {
  const project = mockState.projects.find((entry) => entry.id === projectId);
  if (!project) {
    throw new Error("Project not found");
  }
  return project;
}

function getMockProjectSnapshot(projectId: string) {
  const snapshot = mockState.projectSnapshots.get(projectId);
  if (!snapshot) {
    throw new Error("Project not found");
  }
  return snapshot;
}

function requireWorkspaceSession(projectId: string, workspaceSessionId: string) {
  const session = getMockProjectSnapshot(projectId).sessions.find((entry) => entry.id === workspaceSessionId);
  if (!session) {
    throw new Error("Session not found");
  }
  return session;
}

function touchMockSession(projectId: string, workspaceSessionId: string) {
  const session = requireWorkspaceSession(projectId, workspaceSessionId);
  session.lastOpenedAt = now();
}

function getMockSessionSnapshot(projectId: string, workspaceSessionId: string) {
  requireWorkspaceSession(projectId, workspaceSessionId);
  let snapshot = mockState.sessionSnapshots.get(workspaceSessionId);
  if (!snapshot) {
    snapshot = createEmptySessionSnapshot(projectId, workspaceSessionId);
    mockState.sessionSnapshots.set(workspaceSessionId, snapshot);
  }
  return snapshot;
}

function requireWindow(snapshot: WorkspaceSnapshot, windowId: string) {
  const window = snapshot.windows.find((entry) => entry.id === windowId);
  if (!window) {
    throw new Error("Window not found");
  }
  return window;
}

function ensureMockWindow(snapshot: WorkspaceSnapshot) {
  const existing = snapshot.activeWindowId ? snapshot.windows.find((entry) => entry.id === snapshot.activeWindowId) : null;
  if (existing) {
    return existing;
  }
  const window = createMockTab("main");
  snapshot.windows.push(window);
  snapshot.activeWindowId = window.id;
  return window;
}

function normalizeProject(value: unknown): Project {
  const record = asRecord(value);
  return {
    id: asString(record.id, nextId("project")),
    name: asString(record.name, "Project"),
    path: asString(record.path, "D:\\"),
    color: asString(record.color, "#0ea5e9"),
    icon: typeof record.icon === "string" ? record.icon : null,
    lastOpenedAt: normalizeNullableString(record.lastOpenedAt ?? record.last_opened_at),
    createdAt: asString(record.createdAt ?? record.created_at, now()),
  };
}

function normalizeDeleteProjectResult(value: unknown): DeleteProjectResult {
  const record = asRecord(value);
  return {
    deletedProjectId: asString(record.deletedProjectId ?? record.deleted_project_id, ""),
    nextProjectId: normalizeNullableString(record.nextProjectId ?? record.next_project_id),
  };
}

function normalizeProjectWorkspaceSnapshot(value: unknown, fallbackProjectId: string): ProjectWorkspaceSnapshot {
  const record = asRecord(value);
  return {
    projectId: asString(record.projectId ?? record.project_id, fallbackProjectId),
    sessions: Array.isArray(record.sessions)
      ? record.sessions.map((entry) => normalizeWorkspaceSession(entry, fallbackProjectId))
      : [],
  };
}

function normalizeWorkspaceSession(value: unknown, fallbackProjectId: string): WorkspaceSession {
  const record = asRecord(value);
  return {
    id: asString(record.id, nextId("workspace-session")),
    projectId: asString(record.projectId ?? record.project_id, fallbackProjectId),
    name: asString(record.name, "session"),
    createdBy: record.createdBy === "ai" || record.created_by === "ai" ? "ai" : "user",
    sourceSessionId: normalizeNullableString(record.sourceSessionId ?? record.source_session_id),
    lastOpenedAt: normalizeNullableString(record.lastOpenedAt ?? record.last_opened_at),
    createdAt: asString(record.createdAt ?? record.created_at, now()),
  };
}

function normalizeSessionWorkspaceSnapshot(
  value: unknown,
  fallbackProjectId: string,
  fallbackWorkspaceSessionId: string,
): WorkspaceSnapshot {
  const record = asRecord(value);
  const projectId = asString(record.projectId ?? record.project_id, fallbackProjectId);
  const sessionId = asString(record.sessionId ?? record.session_id, fallbackWorkspaceSessionId);
  const rawWindows = record.windows ?? record.tabs;
  const windowsSource: unknown[] = Array.isArray(rawWindows) ? rawWindows : [];
  const windows = windowsSource.map((entry: unknown, index: number) => normalizeWindow(entry, index + 1));
  const rawTerminals = record.terminals ?? record.sessions;
  const terminalsSource: unknown[] = Array.isArray(rawTerminals)
    ? rawTerminals
    : [];
  return {
    projectId,
    sessionId,
    activeWindowId: asString(record.activeWindowId ?? record.activeTabId ?? record.active_tab_id, windows[0]?.id ?? null),
    windows,
    terminals: terminalsSource.map((entry: unknown) => normalizeTerminalSession(entry, projectId, sessionId)),
  };
}

function normalizeWindow(value: unknown, fallbackOrdinal: number): WorkspaceTab {
  const record = asRecord(value);
  const fallback = createMockTab(fallbackOrdinal === 1 ? "main" : `window-${fallbackOrdinal}`);
  return {
    id: asString(record.id, fallback.id),
    title: asString(record.title, fallback.title),
    root: normalizeLayoutNode(record.root, 1, "user", "unlaunched", null),
    nextPaneOrdinal: asNumber(record.nextPaneOrdinal ?? record.next_pane_ordinal, fallback.nextPaneOrdinal),
    activePaneId: asString(record.activePaneId ?? record.active_pane_id, fallback.activePaneId ?? null),
  };
}

function normalizeLayoutNode(
  value: unknown,
  fallbackOrdinal: number,
  fallbackOwner: "user" | "ai",
  fallbackLaunchState: PaneLaunchState,
  fallbackSourcePaneId: string | null,
): LayoutNode {
  const record = asRecord(value);
  if (record.type === "split") {
    const children = Array.isArray(record.children)
      ? record.children.map((child, index) =>
        normalizeLayoutNode(child, fallbackOrdinal + index, fallbackOwner, fallbackLaunchState, fallbackSourcePaneId))
      : [createEmptyStack(fallbackOrdinal, fallbackOwner, fallbackSourcePaneId, fallbackLaunchState)];
    return {
      type: "split",
      id: asString(record.id, nextId("split")),
      direction: record.direction === "vertical" ? "vertical" : "horizontal",
      zoneKind: record.zoneKind === "aiWorkspace" || record.zone_kind === "aiWorkspace" ? "aiWorkspace" : "default",
      sizes: Array.isArray(record.sizes) ? record.sizes.map((entry) => asNumber(entry, 50)) : [50, 50],
      children,
    };
  }

  const itemsSource = Array.isArray(record.items) ? record.items : [];
  const items = itemsSource.length > 0 ? itemsSource.map(normalizeStackItem) : [emptyStackItem()];
  return {
    type: "stack",
    id: asString(record.id, nextId("stack")),
    paneOrdinal: asNumber(record.paneOrdinal ?? record.pane_ordinal, fallbackOrdinal),
    paneLabel: asString(record.paneLabel ?? record.pane_label, `P${fallbackOrdinal}`),
    createdBy: record.createdBy === "ai" || record.created_by === "ai" ? "ai" : fallbackOwner,
    launchState: record.launchState === "launched" || record.launch_state === "launched" ? "launched" : fallbackLaunchState,
    sourcePaneId: normalizeNullableString(record.sourcePaneId ?? record.source_pane_id) ?? fallbackSourcePaneId,
    activeItemId: asString(record.activeItemId ?? record.active_item_id, items[0].id),
    items,
  };
}

function normalizeStackItem(value: unknown): StackItem {
  const record = asRecord(value);
  return {
    id: asString(record.id, nextId("item")),
    kind: "terminal",
    sessionId: normalizeNullableString(record.sessionId ?? record.session_id),
    title: asString(record.title, "Terminal"),
  };
}

function normalizeTerminalSession(
  value: unknown,
  fallbackProjectId: string,
  fallbackWorkspaceSessionId: string,
): TerminalSession {
  const record = asRecord(value);
  return {
    id: asString(record.id, nextId("terminal-session")),
    projectId: asString(record.projectId ?? record.project_id, fallbackProjectId),
    workspaceSessionId: asString(record.workspaceSessionId ?? record.workspace_session_id, fallbackWorkspaceSessionId),
    windowId: asString(record.windowId ?? record.window_id, ""),
    title: asString(record.title, "Terminal"),
    program: asString(record.program ?? record.shell, "powershell"),
    args: Array.isArray(record.args) ? record.args.filter((entry): entry is string => typeof entry === "string") : null,
    launchProfile: normalizeLaunchProfile(record.launchProfile ?? record.launch_profile),
    tmuxShimEnabled: Boolean(record.tmuxShimEnabled ?? record.tmux_shim_enabled),
    cwd: asString(record.cwd, "D:\\"),
    status: normalizeSessionStatus(record.status),
    startedAt: normalizeNullableString(record.startedAt ?? record.started_at),
    endedAt: normalizeNullableString(record.endedAt ?? record.ended_at),
    exitCode: typeof record.exitCode === "number" ? record.exitCode : typeof record.exit_code === "number" ? record.exit_code : null,
  };
}

function normalizeSessionStatus(value: unknown): TerminalSession["status"] {
  return value === "starting" || value === "running" || value === "exited" || value === "failed" ? value : "failed";
}

function normalizeLaunchProfile(value: unknown): LaunchProfile {
  return value === "claude" || value === "claudeUnsafe" || value === "codex" || value === "codexFullAuto" || value === "terminal"
    ? value
    : "terminal";
}

function normalizePastePayload(value: unknown): PastePayload {
  const record = asRecord(value);
  if (record.kind === "files" && Array.isArray(record.paths)) {
    return { kind: "files", paths: record.paths.filter((entry): entry is string => typeof entry === "string") };
  }
  if (record.kind === "imagePath" && typeof record.imagePath === "string") {
    return { kind: "imagePath", imagePath: record.imagePath };
  }
  if (record.kind === "text" && typeof record.text === "string") {
    return { kind: "text", text: record.text };
  }
  return { kind: "empty" };
}

function splitMockStack(tab: WorkspaceTab, targetId: string, direction: SplitDirection): boolean {
  return splitMockNode(tab.root, tab, targetId, direction);
}

function splitMockNode(node: LayoutNode, tab: WorkspaceTab, targetId: string, direction: SplitDirection): boolean {
  if (node.type === "stack" && node.id === targetId) {
    const current = JSON.parse(JSON.stringify(node)) as LayoutNode;
    Object.keys(node).forEach((key) => delete (node as Record<string, unknown>)[key]);
    Object.assign(node, {
      type: "split",
      id: nextId("split"),
      direction,
      zoneKind: "default",
      sizes: [50, 50],
      children: [current, createEmptyStack(tab.nextPaneOrdinal, "user", targetId)],
    } satisfies LayoutNode);
    tab.nextPaneOrdinal += 1;
    return true;
  }

  return node.type === "split" && node.children.some((child) => splitMockNode(child, tab, targetId, direction));
}

function attachSessionToStack(node: LayoutNode, targetId: string, sessionId: string, title: string): boolean {
  if (node.type === "stack" && node.id === targetId) {
    const itemId = node.items[0]?.id ?? nextId("item");
    node.items = [{ id: itemId, kind: "terminal", sessionId, title }];
    node.activeItemId = itemId;
    node.launchState = "launched";
    return true;
  }

  return node.type === "split" && node.children.some((child) => attachSessionToStack(child, targetId, sessionId, title));
}

function setMockActiveStackItem(node: LayoutNode, targetId: string, itemId: string): boolean {
  if (node.type === "stack" && node.id === targetId && node.items.some((item) => item.id === itemId)) {
    node.activeItemId = itemId;
    return true;
  }
  return node.type === "split" && node.children.some((child) => setMockActiveStackItem(child, targetId, itemId));
}

function closeMockPane(node: LayoutNode, targetId: string): string[] | null {
  if (node.type === "stack" && node.id === targetId) {
    return collectSessionIds(node);
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
        const replacement = node.children[0];
        Object.keys(node).forEach((key) => delete (node as Record<string, unknown>)[key]);
        Object.assign(node, replacement);
      }
      return sessionIds;
    }
    const nested = closeMockPane(child, targetId);
    if (nested) {
      return nested;
    }
  }
  return null;
}

function collectSessionIds(node: LayoutNode): string[] {
  return node.type === "stack"
    ? node.items.flatMap((item) => (item.sessionId ? [item.sessionId] : []))
    : node.children.flatMap((child) => collectSessionIds(child));
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

function asRecord(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" ? (value as Record<string, unknown>) : {};
}

function asString(value: unknown, fallback: string | null): string {
  return typeof value === "string" && value.trim().length > 0 ? value : (fallback ?? "");
}

function asNumber(value: unknown, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function normalizeNullableString(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function emptyStackItem(): StackItem {
  return { id: nextId("item"), kind: "terminal", sessionId: null, title: "Empty" };
}
