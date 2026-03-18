export type Project = {
  id: string;
  name: string;
  path: string;
  color: string;
  icon?: string | null;
  lastOpenedAt?: string | null;
  createdAt: string;
};

export type SessionStatus = "starting" | "running" | "exited" | "failed";

export type TerminalSession = {
  id: string;
  projectId: string;
  title: string;
  program: string;
  args?: string[] | null;
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
  sizes: number[];
  children: LayoutNode[];
};

export type LayoutNode = StackNode | SplitNode;

export type WorkspaceTab = {
  id: string;
  title: string;
  root: LayoutNode;
  nextPaneOrdinal: number;
  activePaneId?: string | null;
};

export type WorkspaceSnapshot = {
  projectId: string;
  activeTabId?: string | null;
  tabs: WorkspaceTab[];
  sessions: TerminalSession[];
};

export type SessionOutputEvent = {
  sessionId: string;
  chunk: string;
};
