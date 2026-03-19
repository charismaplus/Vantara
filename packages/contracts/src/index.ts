export type Project = {
  id: string;
  name: string;
  path: string;
  color: string;
  icon?: string | null;
  lastOpenedAt?: string | null;
  createdAt: string;
};

export type DeleteProjectResult = {
  deletedProjectId: string;
  nextProjectId?: string | null;
};

export type SessionStatus = "starting" | "running" | "exited" | "failed";
export type LaunchProfile = "terminal" | "claude" | "claudeUnsafe" | "codex" | "codexFullAuto";
export type WorkspaceSessionCreatedBy = "user" | "ai";

export type TerminalSession = {
  id: string;
  projectId: string;
  workspaceSessionId: string;
  windowId: string;
  title: string;
  program: string;
  args?: string[] | null;
  launchProfile: LaunchProfile;
  tmuxShimEnabled: boolean;
  cwd: string;
  status: SessionStatus;
  startedAt?: string | null;
  endedAt?: string | null;
  exitCode?: number | null;
};

export type StackItem = {
  id: string;
  kind: "terminal";
  sessionId?: string | null;
  title: string;
};

export type PaneCreatedBy = "user" | "ai";
export type PaneLaunchState = "unlaunched" | "launched";
export type SplitZoneKind = "default" | "aiWorkspace";

export type StackNode = {
  id: string;
  type: "stack";
  paneOrdinal: number;
  paneLabel: string;
  createdBy: PaneCreatedBy;
  launchState: PaneLaunchState;
  sourcePaneId?: string | null;
  activeItemId: string;
  items: StackItem[];
};

export type SplitNode = {
  id: string;
  type: "split";
  direction: "horizontal" | "vertical";
  zoneKind: SplitZoneKind;
  sizes: number[];
  children: LayoutNode[];
};

export type LayoutNode = StackNode | SplitNode;

export type WorkspaceWindow = {
  id: string;
  title: string;
  root: LayoutNode;
  nextPaneOrdinal: number;
  activePaneId?: string | null;
};

export type WorkspaceSession = {
  id: string;
  projectId: string;
  name: string;
  createdBy: WorkspaceSessionCreatedBy;
  sourceSessionId?: string | null;
  lastOpenedAt?: string | null;
  createdAt: string;
};

export type ProjectWorkspaceSnapshot = {
  projectId: string;
  sessions: WorkspaceSession[];
};

export type SessionWorkspaceSnapshot = {
  projectId: string;
  sessionId: string;
  activeWindowId?: string | null;
  windows: WorkspaceWindow[];
  terminals: TerminalSession[];
};

export type SessionOutputEvent = {
  sessionId: string;
  chunk: string;
};

export type WorkspaceChangedEvent = {
  projectId: string;
  sessionId?: string | null;
};

export type WorkspaceTab = WorkspaceWindow;
export type WorkspaceSnapshot = SessionWorkspaceSnapshot;

export type PastePayload =
  | {
      kind: "files";
      paths: string[];
    }
  | {
      kind: "imagePath";
      imagePath: string;
    }
  | {
      kind: "text";
      text: string;
    }
  | {
      kind: "empty";
    };
