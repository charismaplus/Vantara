import type { ReactNode, MouseEvent as ReactMouseEvent } from "react";
import { useEffect, useMemo, useRef, useState } from "react";
import { Group as PanelGroup, Panel, Separator as PanelResizeHandle } from "react-resizable-panels";
import type {
  LayoutNode,
  LaunchProfile,
  Project,
  ProjectWorkspaceSnapshot,
  TerminalSession,
  WorkspaceSession,
  WorkspaceSnapshot,
} from "../../../packages/contracts/src/index.ts";

import { AppErrorBoundary } from "./components/AppErrorBoundary";
import { TerminalView } from "./components/TerminalView";
import {
  closePane,
  closeWindow,
  createProject,
  createSession,
  createWindow,
  createWorkspaceSession,
  deleteProject,
  deleteWorkspaceSession,
  listProjects,
  openProject,
  openSession,
  renameProject,
  renameWindow,
  renameWorkspaceSession,
  reportTabViewport,
  setActiveWindow,
  splitPane,
} from "./lib/api";
import {
  listenSessionExit,
  listenSessionOutput,
  listenWindowFileDrop,
  listenWorkspaceChanged,
  openDirectoryDialog,
} from "./lib/desktop";
import { getActiveTab, isSplitNode, isStackNode } from "./lib/layout";
import { pasteTerminalInput, writeTerminalChunk } from "./lib/terminalRegistry";
import vantaraLogo from "../../../docs/Logo.png";

type LauncherProfile = {
  id: string;
  title: string;
  description: string;
  program: string;
  args?: string[];
  launchProfile: LaunchProfile;
  theme: "claude" | "claude-danger" | "codex" | "codex-auto" | "terminal";
  sessionTitle: string;
};

type SidebarContextMenuState =
  | {
      kind: "project";
      project: Project;
      x: number;
      y: number;
    }
  | {
      kind: "session";
      project: Project;
      session: WorkspaceSession;
      x: number;
      y: number;
    };

const launcherRows: LauncherProfile[][] = [
  [
    { id: "claude", title: "Claude Code", description: "Interactive coding session", program: "claude", launchProfile: "claude", theme: "claude", sessionTitle: "Claude Code" },
    { id: "claude-unsafe", title: "Claude Unsafe", description: "Bypass permissions", program: "claude", args: ["--dangerously-skip-permissions"], launchProfile: "claudeUnsafe", theme: "claude-danger", sessionTitle: "Claude Unsafe" },
  ],
  [
    { id: "codex", title: "Codex", description: "Interactive agent session", program: "codex", launchProfile: "codex", theme: "codex", sessionTitle: "Codex" },
    { id: "codex-full-auto", title: "Codex Full Auto", description: "Workspace-write auto mode", program: "codex", args: ["--full-auto"], launchProfile: "codexFullAuto", theme: "codex-auto", sessionTitle: "Codex Full Auto" },
  ],
  [
    { id: "terminal", title: "Terminal", description: "Plain PowerShell shell", program: "powershell", launchProfile: "terminal", theme: "terminal", sessionTitle: "PowerShell" },
  ],
];

function createSessionMap(snapshot: WorkspaceSnapshot | null) {
  return new Map<string, TerminalSession>(snapshot?.terminals.map((session) => [session.id, session]) ?? []);
}

function getMinimumPanelSize(direction: "horizontal" | "vertical") {
  return direction === "vertical" ? 24 : 20;
}

function createDefaultSplitLayout(node: Extract<LayoutNode, { type: "split" }>) {
  return Object.fromEntries(node.children.map((child, index) => [child.id, node.sizes[index] ?? 50]));
}

function getResolvedSplitLayout(node: Extract<LayoutNode, { type: "split" }>, savedLayout?: Record<string, number>) {
  const fallbackLayout = createDefaultSplitLayout(node);
  if (!savedLayout) {
    return fallbackLayout;
  }
  const childIds = node.children.map((child) => child.id);
  const hasAllChildren = childIds.every((id) => typeof savedLayout[id] === "number");
  return hasAllChildren && Object.keys(savedLayout).length === childIds.length ? savedLayout : fallbackLayout;
}

function collectPaneLabels(node: LayoutNode, paneLabels: Map<string, string>) {
  if (isStackNode(node)) {
    paneLabels.set(node.id, node.paneLabel);
    return;
  }
  if (isSplitNode(node)) {
    for (const child of node.children) {
      collectPaneLabels(child, paneLabels);
    }
  }
}

function withTimeout<T>(promise: Promise<T>, timeoutMs: number, timeoutMessage: string) {
  let timeoutId: number | null = null;
  const timeoutPromise = new Promise<T>((_resolve, reject) => {
    timeoutId = window.setTimeout(() => reject(new Error(timeoutMessage)), timeoutMs);
  });
  return Promise.race([promise, timeoutPromise]).finally(() => {
    if (timeoutId !== null) {
      window.clearTimeout(timeoutId);
    }
  });
}

function getErrorMessage(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

export default function App() {
  const [projects, setProjects] = useState<Project[]>([]);
  const [activeProjectId, setActiveProjectId] = useState<string | null>(null);
  const [projectSnapshot, setProjectSnapshot] = useState<ProjectWorkspaceSnapshot | null>(null);
  const [activeWorkspaceSessionId, setActiveWorkspaceSessionId] = useState<string | null>(null);
  const [sessionSnapshot, setSessionSnapshot] = useState<WorkspaceSnapshot | null>(null);
  const [expandedProjectIds, setExpandedProjectIds] = useState<Record<string, boolean>>({});
  const [isProjectModalOpen, setIsProjectModalOpen] = useState(false);
  const [panelLayouts, setPanelLayouts] = useState<Record<string, Record<string, number>>>({});
  const [newProjectName, setNewProjectName] = useState("");
  const [newProjectPath, setNewProjectPath] = useState("");
  const [loading, setLoading] = useState(false);
  const [errorMessage, setErrorMessage] = useState<string | null>(null);
  const [launchingPaneIds, setLaunchingPaneIds] = useState<string[]>([]);
  const [renamingProjectId, setRenamingProjectId] = useState<string | null>(null);
  const [renamingProjectName, setRenamingProjectName] = useState("");
  const [pendingDeleteProject, setPendingDeleteProject] = useState<Project | null>(null);
  const [pendingDeleteSession, setPendingDeleteSession] = useState<WorkspaceSession | null>(null);
  const [sidebarContextMenu, setSidebarContextMenu] = useState<SidebarContextMenuState | null>(null);
  const workspaceCanvasRef = useRef<HTMLElement | null>(null);
  const activeProjectIdRef = useRef<string | null>(null);
  const activeWorkspaceSessionIdRef = useRef<string | null>(null);
  const refreshProjectSummaryRef = useRef<(projectId: string) => Promise<void>>(async () => {});
  const refreshCurrentSelectionRef = useRef<() => Promise<void>>(async () => {});

  const activeProject = useMemo(() => projects.find((entry) => entry.id === activeProjectId) ?? null, [projects, activeProjectId]);
  const activeWorkspaceSession = useMemo(
    () => projectSnapshot?.sessions.find((entry) => entry.id === activeWorkspaceSessionId) ?? null,
    [projectSnapshot, activeWorkspaceSessionId],
  );
  const sessions = useMemo(() => createSessionMap(sessionSnapshot), [sessionSnapshot]);
  const activeWindow = useMemo(
    () => getActiveTab(sessionSnapshot?.windows ?? [], sessionSnapshot?.activeWindowId),
    [sessionSnapshot],
  );
  const paneLabels = useMemo(() => {
    const labels = new Map<string, string>();
    if (activeWindow) {
      collectPaneLabels(activeWindow.root, labels);
    }
    return labels;
  }, [activeWindow]);

  useEffect(() => {
    activeProjectIdRef.current = activeProjectId;
  }, [activeProjectId]);

  useEffect(() => {
    activeWorkspaceSessionIdRef.current = activeWorkspaceSessionId;
  }, [activeWorkspaceSessionId]);

  useEffect(() => {
    void (async () => {
      try {
        const loadedProjects = await listProjects();
        setProjects(loadedProjects);
        if (loadedProjects[0]) {
          setExpandedProjectIds({ [loadedProjects[0].id]: true });
          setLoading(true);
          activeProjectIdRef.current = loadedProjects[0].id;
          activeWorkspaceSessionIdRef.current = null;
          setActiveProjectId(loadedProjects[0].id);
          setActiveWorkspaceSessionId(null);
          setSessionSnapshot(null);
          setProjectSnapshot(await openProject(loadedProjects[0].id));
          setProjects(await listProjects());
          setLoading(false);
        }
      } catch (error) {
        showError(error);
        setLoading(false);
      }
    })();
  }, []);

  async function refreshProjectSummary(projectId: string) {
    try {
      const nextProjectSnapshot = await openProject(projectId);
      setProjectSnapshot(nextProjectSnapshot);
      setProjects(await listProjects());
    } catch (error) {
      showError(error);
    }
  }

  async function refreshCurrentSelection() {
    const projectId = activeProjectIdRef.current;
    if (!projectId) {
      return;
    }

    setLoading(true);
    try {
      const nextProjectSnapshot = await openProject(projectId);
      setProjectSnapshot(nextProjectSnapshot);
      setProjects(await listProjects());

      const workspaceSessionId = activeWorkspaceSessionIdRef.current;
      if (workspaceSessionId && nextProjectSnapshot.sessions.some((entry) => entry.id === workspaceSessionId)) {
        setSessionSnapshot(await openSession(projectId, workspaceSessionId));
      } else {
        if (workspaceSessionId) {
          activeWorkspaceSessionIdRef.current = null;
          setActiveWorkspaceSessionId(null);
        }
        setSessionSnapshot(null);
      }
    } catch (error) {
      showError(error);
    } finally {
      setLoading(false);
    }
  }

  refreshProjectSummaryRef.current = refreshProjectSummary;
  refreshCurrentSelectionRef.current = refreshCurrentSelection;

  useEffect(() => {
    let disposed = false;
    let outputDispose: (() => void) | null = null;
    let exitDispose: (() => void) | null = null;
    let workspaceDispose: (() => void) | null = null;

    const bindListener = (
      promise: Promise<() => void>,
      assign: (dispose: () => void) => void,
    ) => {
      void promise
        .then((dispose) => {
          if (disposed) {
            dispose();
            return;
          }
          assign(dispose);
        })
        .catch((error) => setErrorMessage(getErrorMessage(error)));
    };

    bindListener(
      listenSessionOutput((event) => writeTerminalChunk(event.payload.sessionId, event.payload.chunk)),
      (dispose) => {
        outputDispose = dispose;
      },
    );

    bindListener(
      listenSessionExit(() => {
        void refreshCurrentSelectionRef.current();
      }),
      (dispose) => {
        exitDispose = dispose;
      },
    );

    bindListener(
      listenWorkspaceChanged((event) => {
        const projectId = activeProjectIdRef.current;
        const workspaceSessionId = activeWorkspaceSessionIdRef.current;

        if (!projectId || event.payload.projectId !== projectId) {
          return;
        }

        if (event.payload.sessionId && workspaceSessionId && event.payload.sessionId !== workspaceSessionId) {
          void refreshProjectSummaryRef.current(projectId);
          return;
        }

        void refreshCurrentSelectionRef.current();
      }),
      (dispose) => {
        workspaceDispose = dispose;
      },
    );

    return () => {
      disposed = true;
      outputDispose?.();
      exitDispose?.();
      workspaceDispose?.();
    };
  }, []);

  useEffect(() => {
    const element = workspaceCanvasRef.current;
    if (!element || !activeProjectId || !activeWindow) {
      return;
    }
    let frame: number | null = null;
    let lastWidth = 0;
    let lastHeight = 0;
    const observer = new ResizeObserver(() => {
      if (frame !== null) {
        cancelAnimationFrame(frame);
      }
      frame = requestAnimationFrame(() => {
        frame = null;
        const rect = element.getBoundingClientRect();
        const width = Math.round(rect.width);
        const height = Math.round(rect.height);
        if (width <= 0 || height <= 0 || (width === lastWidth && height === lastHeight)) {
          return;
        }
        lastWidth = width;
        lastHeight = height;
        void reportTabViewport(activeProjectId, activeWindow.id, width, height).catch(showError);
      });
    });
    observer.observe(element);
    return () => {
      observer.disconnect();
      if (frame !== null) {
        cancelAnimationFrame(frame);
      }
    };
  }, [activeProjectId, activeWindow]);

  useEffect(() => {
    let disposed = false;
    let dispose: (() => void) | null = null;

    void listenWindowFileDrop((event) => {
      if (event.type !== "drop" || !event.paths.length || !event.position) {
        return;
      }
      const scale = window.devicePixelRatio || 1;
      const target = document.elementFromPoint(event.position.x / scale, event.position.y / scale);
      const terminalHost = target?.closest<HTMLElement>("[data-terminal-session-id]");
      const sessionId = terminalHost?.dataset.terminalSessionId;
      if (sessionId) {
        pasteTerminalInput(sessionId, event.paths.join(" "));
      }
    })
      .then((unlisten) => {
        if (disposed) {
          unlisten();
          return;
        }
        dispose = unlisten;
      })
      .catch((error) => setErrorMessage(getErrorMessage(error)));

    return () => {
      disposed = true;
      dispose?.();
    };
  }, []);

  useEffect(() => {
    if (!sidebarContextMenu) {
      return;
    }

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setSidebarContextMenu(null);
      }
    };

    const handleWindowBlur = () => {
      setSidebarContextMenu(null);
    };

    window.addEventListener("keydown", handleKeyDown);
    window.addEventListener("blur", handleWindowBlur);

    return () => {
      window.removeEventListener("keydown", handleKeyDown);
      window.removeEventListener("blur", handleWindowBlur);
    };
  }, [sidebarContextMenu]);

  async function refreshProjectOnly(projectId: string) {
    const nextProjectSnapshot = await openProject(projectId);
    setProjectSnapshot(nextProjectSnapshot);
    const nextProjects = await listProjects();
    setProjects(nextProjects);
    if (activeWorkspaceSessionId && !nextProjectSnapshot.sessions.some((entry) => entry.id === activeWorkspaceSessionId)) {
      activeWorkspaceSessionIdRef.current = null;
      setActiveWorkspaceSessionId(null);
      setSessionSnapshot(null);
    }
  }

  async function selectProject(projectId: string) {
    setLoading(true);
    setErrorMessage(null);
    setSidebarContextMenu(null);
    try {
      activeProjectIdRef.current = projectId;
      activeWorkspaceSessionIdRef.current = null;
      setActiveProjectId(projectId);
      setActiveWorkspaceSessionId(null);
      setSessionSnapshot(null);
      setExpandedProjectIds((current) => ({ ...current, [projectId]: true }));
      setProjectSnapshot(await openProject(projectId));
      setProjects(await listProjects());
    } catch (error) {
      showError(error);
    } finally {
      setLoading(false);
    }
  }

  async function selectWorkspaceSession(projectId: string, workspaceSessionId: string) {
    setLoading(true);
    setErrorMessage(null);
    setSidebarContextMenu(null);
    try {
      activeProjectIdRef.current = projectId;
      activeWorkspaceSessionIdRef.current = workspaceSessionId;
      setActiveProjectId(projectId);
      setActiveWorkspaceSessionId(workspaceSessionId);
      setExpandedProjectIds((current) => ({ ...current, [projectId]: true }));
      const [nextProjectSnapshot, nextSessionSnapshot, nextProjects] = await Promise.all([
        openProject(projectId),
        openSession(projectId, workspaceSessionId),
        listProjects(),
      ]);
      setProjectSnapshot(nextProjectSnapshot);
      setSessionSnapshot(nextSessionSnapshot);
      setProjects(nextProjects);
    } catch (error) {
      showError(error);
    } finally {
      setLoading(false);
    }
  }

  async function refreshSessionSnapshot(promise: Promise<WorkspaceSnapshot>) {
    try {
      setErrorMessage(null);
      const nextSnapshot = await promise;
      setSessionSnapshot(nextSnapshot);
      if (activeProjectId) {
        setProjectSnapshot(await openProject(activeProjectId));
      }
    } catch (error) {
      showError(error);
    }
  }

  async function submitProject() {
    if (!newProjectName.trim() || !newProjectPath.trim()) {
      return;
    }
    try {
      const project = await createProject(newProjectName.trim(), newProjectPath.trim());
      setIsProjectModalOpen(false);
      setNewProjectName("");
      setNewProjectPath("");
      await selectProject(project.id);
    } catch (error) {
      showError(error);
    }
  }

  async function browseForProjectPath() {
    try {
      const selected = await openDirectoryDialog();
      if (selected) {
        setNewProjectPath(selected);
      }
    } catch (error) {
      showError(error);
    }
  }

  async function launchFromPane(paneId: string, profile: LauncherProfile) {
    if (!activeProjectId || !activeWorkspaceSessionId || !activeWindow || launchingPaneIds.includes(paneId)) {
      return;
    }
    setLaunchingPaneIds((current) => [...current, paneId]);
    try {
      await refreshSessionSnapshot(withTimeout(createSession({
        projectId: activeProjectId,
        workspaceSessionId: activeWorkspaceSessionId,
        windowId: activeWindow.id,
        stackId: paneId,
        title: profile.sessionTitle,
        program: profile.program,
        args: profile.args,
        launchProfile: profile.launchProfile,
      }), 12000, "Session start timed out. The launcher was unlocked so you can try again."));
    } finally {
      setLaunchingPaneIds((current) => current.filter((entry) => entry !== paneId));
    }
  }

  function showError(error: unknown) {
    setErrorMessage(getErrorMessage(error));
  }

  function resolveContextMenuPosition(clientX: number, clientY: number) {
    const menuWidth = 196;
    const menuHeight = 172;
    const padding = 10;
    return {
      x: Math.max(padding, Math.min(clientX, window.innerWidth - menuWidth - padding)),
      y: Math.max(padding, Math.min(clientY, window.innerHeight - menuHeight - padding)),
    };
  }

  function openProjectContextMenu(event: ReactMouseEvent<HTMLElement>, project: Project) {
    event.preventDefault();
    const position = resolveContextMenuPosition(event.clientX, event.clientY);
    setSidebarContextMenu({
      kind: "project",
      project,
      x: position.x,
      y: position.y,
    });
  }

  function openSessionContextMenu(event: ReactMouseEvent<HTMLElement>, project: Project, session: WorkspaceSession) {
    event.preventDefault();
    const position = resolveContextMenuPosition(event.clientX, event.clientY);
    setSidebarContextMenu({
      kind: "session",
      project,
      session,
      x: position.x,
      y: position.y,
    });
  }

  async function commitProjectRename(projectId: string) {
    const nextName = renamingProjectName.trim();
    if (!nextName) {
      showError(new Error("Project name is required"));
      setRenamingProjectId(null);
      setRenamingProjectName("");
      return;
    }
    await renameProject(projectId, nextName);
    setRenamingProjectId(null);
    setRenamingProjectName("");
    await refreshProjectOnly(projectId);
  }

  async function createAndOpenSession(projectId: string) {
    const session = await createWorkspaceSession(projectId);
    await selectWorkspaceSession(projectId, session.id);
  }

  async function handleProjectContextAction(action: "rename" | "delete", project: Project) {
    setSidebarContextMenu(null);
    if (action === "rename") {
      setRenamingProjectId(project.id);
      setRenamingProjectName(project.name);
      return;
    }
    setPendingDeleteProject(project);
  }

  async function handleSessionContextAction(action: "open" | "rename" | "delete", project: Project, session: WorkspaceSession) {
    setSidebarContextMenu(null);
    if (action === "open") {
      await selectWorkspaceSession(project.id, session.id);
      return;
    }
    if (action === "rename") {
      const nextName = window.prompt("Rename session", session.name);
      if (!nextName?.trim()) {
        return;
      }
      try {
        await renameWorkspaceSession(project.id, session.id, nextName.trim());
        await refreshProjectOnly(project.id);
      } catch (error) {
        showError(error);
      }
      return;
    }
    setPendingDeleteSession(session);
  }

  function renderLauncherSurface(onLaunch: (profile: LauncherProfile) => void) {
    return (
      <div className="launcher-surface">
        <div className="launcher-header">
          <div>
            <p className="eyebrow">Launch Surface</p>
            <h3>{activeWorkspaceSession?.name ?? activeProject?.name ?? "Workspace Session"}</h3>
          </div>
        </div>
        <p className="launcher-path" title={activeProject?.path ?? ""}>{activeProject?.path ?? ""}</p>
        <div className="launcher-grid">
          {launcherRows.map((row, rowIndex) => (
            <div key={`launcher-row-${rowIndex + 1}`} className={`launcher-row launcher-row-${rowIndex + 1}`}>
              {row.map((profile) => (
                <button key={profile.id} className={`launcher-tile ${profile.theme}`} onClick={() => void onLaunch(profile)}>
                  <span className="launcher-tile-title">{profile.title}</span>
                  <span className="launcher-tile-desc">{profile.description}</span>
                </button>
              ))}
            </div>
          ))}
        </div>
      </div>
    );
  }

  function renderNode(node: LayoutNode, canClosePane = false): ReactNode {
    if (isSplitNode(node)) {
      const splitLayout = getResolvedSplitLayout(node, panelLayouts[node.id]);
      const splitKey = `${node.id}:${node.children.map((child) => child.id).join(":")}`;
      return (
        <PanelGroup
          key={splitKey}
          id={node.id}
          orientation={node.direction === "horizontal" ? "horizontal" : "vertical"}
          defaultLayout={splitLayout}
          onLayoutChanged={(sizes) => setPanelLayouts((current) => ({ ...current, [node.id]: sizes }))}
          className="panel-group"
        >
          {node.children.map((child) => (
            <Panel key={child.id} id={child.id} minSize={getMinimumPanelSize(node.direction)} defaultSize={splitLayout[child.id] ?? undefined}>
              {renderNode(child, true)}
            </Panel>
          )).flatMap((panel, index, array) => index < array.length - 1 ? [panel, <PanelResizeHandle key={`resize-${index}`} className="resize-handle" />] : [panel])}
        </PanelGroup>
      );
    }

    const activeItem = node.items.find((item) => item.id === node.activeItemId) ?? node.items[0];
    const session = activeItem?.sessionId ? sessions.get(activeItem.sessionId) : undefined;
    const sourcePaneLabel = node.sourcePaneId ? paneLabels.get(node.sourcePaneId) : null;
    const originLabel = node.createdBy === "ai" ? (sourcePaneLabel ? `AI from ${sourcePaneLabel}` : "AI") : null;

    return (
      <div className={`stack-pane ${node.createdBy === "ai" ? "ai-pane" : "user-pane"}`}>
        <div className="stack-toolbar">
          <div className="stack-primary">
            <span className={`pane-badge ${node.createdBy === "ai" ? "ai" : "user"}`}>{node.paneLabel}</span>
            {originLabel ? <span className="pane-origin">{originLabel}</span> : null}
            <div className="pane-label" title={activeItem?.title ?? "Pane"}>{activeItem?.title ?? "Pane"}</div>
          </div>
          <div className="stack-actions">
            {session ? <span className={`status-pill toolbar-status ${session.status}`}>{session.status}</span> : null}
            <button className="toolbar-button" title="Split Up/Down" onClick={() => activeProjectId && activeWorkspaceSessionId && activeWindow && void refreshSessionSnapshot(splitPane(activeProjectId, activeWorkspaceSessionId, activeWindow.id, node.id, "vertical"))}>━</button>
            <button className="toolbar-button" title="Split Left/Right" onClick={() => activeProjectId && activeWorkspaceSessionId && activeWindow && void refreshSessionSnapshot(splitPane(activeProjectId, activeWorkspaceSessionId, activeWindow.id, node.id, "horizontal"))}>┃</button>
            {canClosePane ? <button className="toolbar-button subtle" onClick={() => activeProjectId && activeWorkspaceSessionId && activeWindow && void refreshSessionSnapshot(closePane(activeProjectId, activeWorkspaceSessionId, activeWindow.id, node.id))}>Close Pane</button> : null}
          </div>
        </div>
        <div className="stack-body">
          {session && session.status === "running" ? (
            <TerminalView sessionId={session.id} variant={node.createdBy === "ai" ? "ai" : "user"} />
          ) : node.createdBy === "user" && node.launchState === "unlaunched" ? (
            renderLauncherSurface((profile) => launchFromPane(node.id, profile))
          ) : session ? (
            <div className="empty-stack">
              <p>{`${node.paneLabel} · ${session.title}`}</p>
              <p>Session is {session.status}. Start a new terminal in this pane.</p>
              <button className="toolbar-button primary" onClick={() => activeProjectId && activeWorkspaceSessionId && activeWindow && void refreshSessionSnapshot(createSession({
                projectId: activeProjectId,
                workspaceSessionId: activeWorkspaceSessionId,
                windowId: activeWindow.id,
                stackId: node.id,
                title: session.title,
                program: session.program,
                args: session.args ?? undefined,
                launchProfile: session.launchProfile,
              }))}>Start Replacement Session</button>
            </div>
          ) : (
            <div className="empty-stack">
              <p>{`${node.paneLabel} · No session attached`}</p>
              <button className="toolbar-button primary" onClick={() => activeProjectId && activeWorkspaceSessionId && activeWindow && void refreshSessionSnapshot(createSession({
                projectId: activeProjectId,
                workspaceSessionId: activeWorkspaceSessionId,
                windowId: activeWindow.id,
                stackId: node.id,
                launchProfile: "terminal",
              }))}>Start Terminal</button>
            </div>
          )}
        </div>
      </div>
    );
  }

  return (
    <AppErrorBoundary>
      <div className="app-shell">
        <aside className="sidebar">
          <div className="sidebar-header">
            <div className="sidebar-brand">
              <img className="sidebar-logo" src={vantaraLogo} alt="Vantara" />
            </div>
            <div className="sidebar-section-header">
              <h1>Projects</h1>
              <button className="toolbar-button primary" onClick={() => setIsProjectModalOpen(true)}>Add Project</button>
            </div>
          </div>
          {errorMessage ? <div className="error-banner">{errorMessage}</div> : null}
          <div className="sidebar-tree">
            {projects.map((project) => {
              const projectSessions = projectSnapshot?.projectId === project.id ? projectSnapshot.sessions : [];
              return (
                <div key={project.id} className={`tree-group ${project.id === activeProjectId ? "active" : ""}`}>
                  {renamingProjectId === project.id ? (
                    <div className="tree-row tree-row-project tree-row-static">
                      <span className="project-color" style={{ background: project.color }} />
                      <div className="tree-inline-edit">
                        <input
                          className="project-inline-input"
                          value={renamingProjectName}
                          onChange={(event) => setRenamingProjectName(event.target.value)}
                          onBlur={() => void commitProjectRename(project.id).catch(showError)}
                          onKeyDown={(event) => {
                            if (event.key === "Enter") {
                              void commitProjectRename(project.id).catch(showError);
                            } else if (event.key === "Escape") {
                              setRenamingProjectId(null);
                              setRenamingProjectName("");
                            }
                          }}
                          autoFocus
                        />
                      </div>
                    </div>
                  ) : (
                    <>
                      <div
                        className={`tree-row tree-row-project ${project.id === activeProjectId ? "active" : ""}`}
                        onClick={() => void selectProject(project.id)}
                        onContextMenu={(event) => openProjectContextMenu(event, project)}
                      >
                        <span className="project-color" style={{ background: project.color }} />
                        <button
                          className={`tree-disclosure ${expandedProjectIds[project.id] ? "expanded" : ""}`}
                          aria-label={expandedProjectIds[project.id] ? "Collapse sessions" : "Expand sessions"}
                          onClick={(event) => {
                            event.stopPropagation();
                            setExpandedProjectIds((current) => ({ ...current, [project.id]: !current[project.id] }));
                          }}
                        >
                          <span className="tree-disclosure-glyph">{">"}</span>
                        </button>
                        <div className="tree-row-body">
                          <strong>{project.name}</strong>
                          <div className="project-path">{project.path}</div>
                        </div>
                        <span className="tree-row-surface">Project</span>
                      </div>
                      {expandedProjectIds[project.id] ? (
                        <div className="tree-children">
                          {projectSessions.map((session) => (
                            <div
                              key={session.id}
                              className={`tree-row tree-row-session ${session.id === activeWorkspaceSessionId ? "active" : ""}`}
                              onClick={() => void selectWorkspaceSession(project.id, session.id)}
                              onContextMenu={(event) => openSessionContextMenu(event, project, session)}
                            >
                              <span className="tree-branch" />
                              <span className={`tree-session-badge ${session.createdBy === "ai" ? "ai" : "user"}`}>
                                {session.createdBy === "ai" ? "AI" : "S"}
                              </span>
                              <div className="tree-row-body">
                                <strong>{session.name}</strong>
                                <div className="project-path">{session.createdBy === "ai" ? "AI session" : "Session"}</div>
                              </div>
                              <span className="tree-row-surface">Session</span>
                            </div>
                          ))}
                          <div className="tree-inline-action-row">
                            <span className="tree-branch" />
                            <button
                              className="tree-inline-action-button"
                              onClick={() => void createAndOpenSession(project.id).catch(showError)}
                            >
                              New Session
                            </button>
                          </div>
                        </div>
                      ) : null}
                    </>
                  )}
                </div>
              );
            })}
          </div>
        </aside>

        <main className="workspace">
          <header className="workspace-header">
            <div>
              <p className="eyebrow">{activeWorkspaceSession ? "Session Workspace" : "Project Sessions"}</p>
              <h2>{activeWorkspaceSession?.name ?? activeProject?.name ?? "No project selected"}</h2>
            </div>
            {activeProjectId && activeWorkspaceSession ? (
              <button className="toolbar-button primary" onClick={() => void refreshSessionSnapshot(createWindow(activeProjectId, activeWorkspaceSession.id))}>New Window</button>
            ) : null}
          </header>

          {activeWorkspaceSession && sessionSnapshot ? (
            <>
              <div className="workspace-tabs">
                {sessionSnapshot.windows.map((windowTab) => (
                  <button key={windowTab.id} className={`workspace-tab ${sessionSnapshot.activeWindowId === windowTab.id ? "active" : ""}`} onClick={() => activeProjectId && void refreshSessionSnapshot(setActiveWindow(activeProjectId, activeWorkspaceSession.id, windowTab.id))} onDoubleClick={() => { const nextTitle = window.prompt("Rename window", windowTab.title); if (nextTitle?.trim() && activeProjectId) { void refreshSessionSnapshot(renameWindow(activeProjectId, activeWorkspaceSession.id, windowTab.id, nextTitle.trim())); } }}>
                    <span>{windowTab.title}</span>
                    <span className="workspace-tab-close" onClick={(event) => { event.stopPropagation(); if (activeProjectId) { void refreshSessionSnapshot(closeWindow(activeProjectId, activeWorkspaceSession.id, windowTab.id)); } }}>x</span>
                  </button>
                ))}
              </div>
              <section className="workspace-canvas" ref={workspaceCanvasRef} onDragOver={(event) => event.preventDefault()} onDrop={(event) => event.preventDefault()}>
                {loading ? <div className="empty-state">Loading workspace...</div> : activeWindow ? renderNode(activeWindow.root, false) : <div className="empty-state">Preparing session window...</div>}
              </section>
            </>
          ) : (
            <section className="workspace-canvas">
              {loading ? <div className="empty-state">Loading project...</div> : activeProject ? (
                <div className="project-list">
                  {(projectSnapshot?.sessions ?? []).map((session) => (
                    <div key={session.id} className="session-list-card">
                      <button className="session-list-card-main" onClick={() => void selectWorkspaceSession(activeProject.id, session.id)}>
                        <div className="session-list-card-content">
                          <strong>{session.name}</strong>
                          <div className="session-list-card-meta">
                            <span className={`tree-session-badge ${session.createdBy === "ai" ? "ai" : "user"}`}>
                              {session.createdBy === "ai" ? "AI" : "S"}
                            </span>
                            <span>{session.createdBy === "ai" ? "AI generated session" : "User session"}</span>
                          </div>
                        </div>
                      </button>
                    </div>
                  ))}
                </div>
              ) : <div className="empty-state">Create a project to begin.</div>}
            </section>
          )}
        </main>

        {isProjectModalOpen ? <div className="modal-backdrop" onClick={(event) => event.target === event.currentTarget && setIsProjectModalOpen(false)}><div className="modal-card"><div className="modal-header"><div><p className="eyebrow">New Project</p><h3>Add a local workspace</h3></div><button className="toolbar-button" onClick={() => setIsProjectModalOpen(false)}>Close</button></div><div className="project-form modal-form"><input placeholder="Project name" value={newProjectName} onChange={(event) => setNewProjectName(event.target.value)} autoFocus /><div className="path-field"><input placeholder="Absolute path" value={newProjectPath} onChange={(event) => setNewProjectPath(event.target.value)} /><button className="toolbar-button" onClick={() => void browseForProjectPath()}>Browse</button></div><div className="modal-actions"><button className="toolbar-button" onClick={() => setIsProjectModalOpen(false)}>Cancel</button><button className="toolbar-button primary" onClick={() => void submitProject()}>Add Project</button></div></div></div></div> : null}

        {pendingDeleteProject ? <div className="modal-backdrop" onClick={(event) => event.target === event.currentTarget && setPendingDeleteProject(null)}><div className="modal-card modal-card-compact"><div className="modal-header"><div><p className="eyebrow">Remove Project</p><h3>{pendingDeleteProject.name}</h3></div><button className="toolbar-button" onClick={() => setPendingDeleteProject(null)}>Close</button></div><p className="modal-copy">This removes the project from Vantara only. The actual folder stays on disk.</p><div className="modal-actions"><button className="toolbar-button" onClick={() => setPendingDeleteProject(null)}>Cancel</button><button className="toolbar-button subtle" onClick={() => deleteProject(pendingDeleteProject.id).then(async (result) => { setPendingDeleteProject(null); const nextProjects = await listProjects(); setProjects(nextProjects); if (result.nextProjectId) { await selectProject(result.nextProjectId); } else { activeProjectIdRef.current = null; activeWorkspaceSessionIdRef.current = null; setActiveProjectId(null); setProjectSnapshot(null); setActiveWorkspaceSessionId(null); setSessionSnapshot(null); } }).catch(showError)}>Delete Project</button></div></div></div> : null}

        {pendingDeleteSession && activeProjectId ? <div className="modal-backdrop" onClick={(event) => event.target === event.currentTarget && setPendingDeleteSession(null)}><div className="modal-card modal-card-compact"><div className="modal-header"><div><p className="eyebrow">Remove Session</p><h3>{pendingDeleteSession.name}</h3></div><button className="toolbar-button" onClick={() => setPendingDeleteSession(null)}>Close</button></div><p className="modal-copy">This removes the session node and its runtime terminals. The project remains intact.</p><div className="modal-actions"><button className="toolbar-button" onClick={() => setPendingDeleteSession(null)}>Cancel</button><button className="toolbar-button subtle" onClick={() => deleteWorkspaceSession(activeProjectId, pendingDeleteSession.id).then((nextSnapshot) => { setPendingDeleteSession(null); setProjectSnapshot(nextSnapshot); if (activeWorkspaceSessionId === pendingDeleteSession.id) { activeWorkspaceSessionIdRef.current = null; setActiveWorkspaceSessionId(null); setSessionSnapshot(null); } }).catch(showError)}>Delete Session</button></div></div></div> : null}

        {sidebarContextMenu ? (
          <div className="context-menu-layer" onClick={() => setSidebarContextMenu(null)} onContextMenu={(event) => event.preventDefault()}>
            <div
              className="context-menu"
              style={{ left: sidebarContextMenu.x, top: sidebarContextMenu.y }}
              onClick={(event) => event.stopPropagation()}
              onContextMenu={(event) => event.preventDefault()}
            >
              {sidebarContextMenu.kind === "project" ? (
                <>
                  <button className="context-menu-item" onClick={() => void handleProjectContextAction("rename", sidebarContextMenu.project)}>Rename Project</button>
                  <button className="context-menu-item danger" onClick={() => void handleProjectContextAction("delete", sidebarContextMenu.project)}>Delete Project</button>
                </>
              ) : (
                <>
                  <button className="context-menu-item" onClick={() => void handleSessionContextAction("open", sidebarContextMenu.project, sidebarContextMenu.session)}>Open Session</button>
                  <button className="context-menu-item" onClick={() => void handleSessionContextAction("rename", sidebarContextMenu.project, sidebarContextMenu.session)}>Rename Session</button>
                  <button className="context-menu-item danger" onClick={() => void handleSessionContextAction("delete", sidebarContextMenu.project, sidebarContextMenu.session)}>Delete Session</button>
                </>
              )}
            </div>
          </div>
        ) : null}
      </div>
    </AppErrorBoundary>
  );
}
